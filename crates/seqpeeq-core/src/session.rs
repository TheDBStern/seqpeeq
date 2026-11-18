//! A session: the open datasets, and the one place commands are executed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::{Timestamp, Zoned};

use crate::bam::{BamDoc, BamExport, Reference};
use crate::log::stamp;
use crate::msa::layer::Layer;
use crate::msa::{Alignment, Applied, Extraction, count, write};
use crate::pair::Pair;
use crate::progress::Progress;
use crate::tree::newick::save_newick;
use crate::tree::reroot::some_names;
use crate::tree::{Tree, TreeDoc, TreeExtraction};
use crate::{BamCmd, Command, Error, Kind, MsaCmd, Result, SessionLog, TreeCmd};

/// What an open file holds.
#[derive(Debug)]
pub enum Content {
    // Boxed so that a list of datasets stays small whatever they hold.
    Msa(Box<Alignment>),
    Tree(Box<TreeDoc>),
    Bam(Box<BamDoc>),
    /// A kind of file that has no viewer yet; only its path and size are known.
    Unloaded,
}

/// A file opened in the session.
#[derive(Debug)]
pub struct Dataset {
    /// A number for this file within the session: 1, 2, 3 … in order of
    /// opening. Unlike its position among the open files, it never changes
    /// and is never given to another file.
    pub id: u64,
    pub path: PathBuf,
    pub kind: Kind,
    /// The size of the file on disk when it was opened.
    pub size: u64,
    pub content: Content,
}

impl Dataset {
    /// The alignment, if this file is one.
    pub fn msa(&self) -> Option<&Alignment> {
        match &self.content {
            Content::Msa(alignment) => Some(alignment),
            Content::Tree(_) | Content::Bam(_) | Content::Unloaded => None,
        }
    }

    /// The tree, if this file is one.
    pub fn tree(&self) -> Option<&TreeDoc> {
        match &self.content {
            Content::Tree(tree) => Some(tree),
            Content::Msa(_) | Content::Bam(_) | Content::Unloaded => None,
        }
    }

    /// The BAM, if this file is one.
    pub fn bam(&self) -> Option<&BamDoc> {
        match &self.content {
            Content::Bam(bam) => Some(bam),
            Content::Msa(_) | Content::Tree(_) | Content::Unloaded => None,
        }
    }

    /// Whether the file has edits not written whole to another file.
    pub fn is_modified(&self) -> bool {
        match &self.content {
            Content::Msa(alignment) => alignment.is_modified(),
            Content::Tree(tree) => tree.is_modified(),
            Content::Bam(_) | Content::Unloaded => false,
        }
    }

    /// The file's name without its directory, for messages.
    fn file_name(&self) -> String {
        file_name(&self.path)
    }
}

/// A path's last part, for messages.
fn file_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// What kind of file `path` is, or why it cannot be opened: a kind that
/// cannot be told from the name comes first, then a file that cannot be read.
pub fn check_input(path: &Path, kind: Option<Kind>) -> Result<Kind> {
    let kind = kind
        .or_else(|| Kind::from_path(path))
        .ok_or_else(|| Error::UnknownKind(path.to_path_buf()))?;
    std::fs::metadata(path).map_err(|cause| Error::Io {
        path: path.to_path_buf(),
        cause,
    })?;
    Ok(kind)
}

/// What is known about a file before it is read.
struct Input {
    path: PathBuf,
    kind: Kind,
    size: u64,
    /// The comment written to the log after the `open`.
    note: String,
    /// Where an index goes when the file's own folder cannot take it.
    cache: PathBuf,
}

impl Input {
    fn check(path: &Path, kind: Option<Kind>, now: &Zoned) -> Result<Input> {
        let kind = check_input(path, kind)?;
        let io = |cause| Error::Io {
            path: path.to_path_buf(),
            cause,
        };
        let meta = std::fs::metadata(path).map_err(io)?;
        let absolute = std::fs::canonicalize(path).map_err(io)?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok())
            .map(|t| stamp(&t.to_zoned(now.time_zone().clone())))
            .unwrap_or_else(|| "unknown".to_string());
        let size = meta.len();
        Ok(Input {
            path: path.to_path_buf(),
            kind,
            size,
            note: format!("input {} size={size} mtime={modified}", absolute.display()),
            cache: PathBuf::new(),
        })
    }

    /// The message for a file that was opened and holds `holds`.
    fn opened(&self, holds: &str) -> String {
        format!(
            "opened {} ({}, {holds})",
            self.path.display(),
            self.kind.as_str()
        )
    }
}

/// A link's counts for messages: `3 of 3 tips have a sequence`, then the
/// names on either side that have no match, each part starting `; `.
struct LinkCounts {
    matched: String,
    unmatched: String,
}

/// How a tree's tips match the alignment it is linked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkSummary {
    /// The alignment's dataset id.
    pub alignment: u64,
    /// Tips whose name is a sequence name in the alignment.
    pub matched: usize,
    pub tips: usize,
}

/// What a successfully executed command reports back to the interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub message: String,
}

/// What [`Session::start`] did with a command.
pub enum Started {
    /// A quick command: carried out and recorded.
    Done(Outcome),
    /// A slow command: checked, with its slow part still to run.
    Running(Job),
}

/// The slow part of a command. It holds no reference to the session, so it
/// can run on another thread while the session stays where it is. Nothing
/// in the session changes until the result is passed to [`Session::finish`].
pub struct Job {
    label: String,
    line: String,
    progress: Arc<Progress>,
    // Boxed so that a job is small to pass around, whatever it carries.
    work: Box<Work>,
}

enum Work {
    Open(Input),
    Extract {
        /// The id of the dataset being written.
        dataset: u64,
        extraction: Extraction,
    },
    TreeExtract {
        dataset: u64,
        extraction: Box<TreeExtraction>,
    },
    /// Read a reference FASTA's index, for `bam ref`.
    BamRef {
        dataset: u64,
        file: PathBuf,
    },
    /// Write a BAM region or a stretch of the reference.
    BamExport {
        dataset: u64,
        export: Box<BamExport>,
    },
}

impl Job {
    /// What is being done, such as `opening a.fa`.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// How far the work has got, and the way to cancel it.
    pub fn progress(&self) -> Arc<Progress> {
        Arc::clone(&self.progress)
    }

    /// Do the work.
    pub fn run(self) -> Finished {
        let result = match *self.work {
            Work::Open(input) => open_content(&input, &self.progress).map(|(content, holds)| {
                let mut message = input.opened(&holds[0]);
                for note in &holds[1..] {
                    message.push_str("; ");
                    message.push_str(note);
                }
                Product::Opened {
                    input,
                    content,
                    message,
                }
            }),
            Work::Extract {
                dataset,
                extraction,
            } => extraction
                .run(&self.progress)
                .map(|applied| Product::Wrote {
                    dataset,
                    applied,
                    whole: true,
                }),
            Work::TreeExtract {
                dataset,
                extraction,
            } => extraction
                .run(&self.progress)
                .map(|applied| Product::Wrote {
                    dataset,
                    applied,
                    whole: extraction.is_whole(),
                }),
            Work::BamExport { dataset, export } => {
                export.run(&self.progress).map(|applied| Product::Wrote {
                    dataset,
                    applied,
                    whole: false,
                })
            }
            Work::BamRef { dataset, file } => {
                Reference::open(&file, &self.progress).map(|reference| Product::Attached {
                    dataset,
                    reference: Box::new(reference),
                })
            }
        };
        Finished {
            line: self.line,
            result,
        }
    }
}

/// The result of a [`Job`], to be handed back to the session.
pub struct Finished {
    line: String,
    result: Result<Product>,
}

/// Read a file of a kind that has a viewer. Returns it with what the open
/// message says: what it holds, then any notes.
fn open_content(input: &Input, progress: &Progress) -> Result<(Content, Vec<String>)> {
    match input.kind {
        Kind::Msa => {
            let alignment = Alignment::load_with(&input.path, progress)?;
            let holds = vec![alignment.shape()];
            Ok((Content::Msa(Box::new(alignment)), holds))
        }
        Kind::Tree => {
            let tree = TreeDoc::load_with(&input.path, progress)?;
            let mut holds = vec![tree.shape()];
            holds.extend(tree.notes().iter().cloned());
            Ok((Content::Tree(Box::new(tree)), holds))
        }
        Kind::Bam => {
            let bam = BamDoc::open(&input.path, &input.cache, progress)?;
            let mut holds = vec![bam.shape()];
            holds.extend(bam.notes().iter().cloned());
            Ok((Content::Bam(Box::new(bam)), holds))
        }
    }
}

enum Product {
    /// A reference read for the BAM `dataset`.
    Attached {
        dataset: u64,
        reference: Box<Reference>,
    },
    Opened {
        input: Input,
        content: Content,
        message: String,
    },
    Wrote {
        dataset: u64,
        applied: Applied,
        /// Whether the whole file was written, so its edits are saved.
        whole: bool,
    },
}

pub struct Session {
    datasets: Vec<Dataset>,
    /// The dataset that data commands apply to.
    current: Option<usize>,
    /// The id the next opened file will get.
    next_id: u64,
    /// The label of the job handed out and not yet finished.
    busy: Option<String>,
    history: Vec<String>,
    log: Option<SessionLog>,
    clock: fn() -> Zoned,
    /// Trees linked to alignments, by dataset id.
    links: HashMap<u64, u64>,
    /// The id the next edit to two files will tag both its steps with.
    next_pair: u64,
    /// Where a BAM's index goes when the BAM's own folder cannot take it.
    cache_dir: PathBuf,
}

/// `$XDG_CACHE_HOME/seqpeeq`, else `~/.cache/seqpeeq`, else a folder in
/// the system's temporary folder.
fn default_cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("seqpeeq")
}

/// Enough of the state before a command to put it back if the command
/// cannot be logged.
struct Checkpoint {
    datasets: usize,
    current: Option<usize>,
    /// The state of each file the command may change, by position: the
    /// current file and, for an edit to two files, the other one.
    snapshots: Vec<(usize, Snapshot)>,
    links: HashMap<u64, u64>,
}

enum Snapshot {
    Msa(Layer),
    Tree((Tree, u64)),
}

impl Session {
    /// A session that logs to `log`, or does not log when it is `None`.
    pub fn new(log: Option<SessionLog>) -> Session {
        Session::with_clock(log, Zoned::now)
    }

    /// As [`Session::new`], with the source of timestamps supplied (for tests).
    pub fn with_clock(log: Option<SessionLog>, clock: fn() -> Zoned) -> Session {
        Session {
            datasets: Vec::new(),
            current: None,
            next_id: 1,
            busy: None,
            history: Vec::new(),
            log,
            clock,
            links: HashMap::new(),
            next_pair: 1,
            cache_dir: default_cache_dir(),
        }
    }

    /// Keep BAM indexes that cannot go beside their BAM in `dir` instead of
    /// the default cache folder.
    pub fn set_cache_dir(&mut self, dir: PathBuf) {
        self.cache_dir = dir;
    }

    /// Run a command to its end and, if it succeeds, record it.
    pub fn execute(&mut self, cmd: Command) -> Result<Outcome> {
        match self.start(cmd)? {
            Started::Done(outcome) => Ok(outcome),
            Started::Running(job) => self.finish(job.run()),
        }
    }

    /// Begin a command. A quick one is carried out and recorded at once. A
    /// slow one (opening an alignment, writing a file) is checked and
    /// returned as a [`Job`]; until that job's result is passed to
    /// [`Session::finish`] the session accepts no other command, so the log
    /// stays in the order things happened.
    ///
    /// The log is opened before anything is applied, so a session that
    /// cannot be logged refuses the command instead of changing state
    /// silently.
    pub fn start(&mut self, cmd: Command) -> Result<Started> {
        if let Some(label) = &self.busy {
            return Err(Error::Invalid(format!("still busy: {label}")));
        }
        let line = cmd.to_line()?;
        // The log is replayed from this text, so it must mean this command.
        if Command::parse_line(&line).ok().as_ref() != Some(&cmd) {
            return Err(Error::Invalid(format!(
                "internal error: '{line}' does not read back as the same command; nothing was done"
            )));
        }
        let now = (self.clock)();
        if let Some(log) = &mut self.log {
            log.ensure_open(&now)?;
        }

        let cmd = match cmd {
            Command::Open { path, kind } => {
                let mut input = Input::check(&path, kind, &now)?;
                input.cache = self.cache_dir.clone();
                let label = format!("opening {}", file_name(&input.path));
                return Ok(self.hand_out(label, line, Work::Open(input)));
            }
            Command::Msa(MsaCmd::Extract {
                rows,
                cols,
                wrap,
                out,
            }) => {
                let extraction =
                    self.current_alignment()?
                        .plan_extract(&rows, cols.as_ref(), wrap, &out)?;
                let dataset = self.datasets[self.current.expect("there is a current file")].id;
                let label = format!("writing {}", file_name(extraction.out()));
                let work = Work::Extract {
                    dataset,
                    extraction,
                };
                return Ok(self.hand_out(label, line, work));
            }
            Command::Tree(TreeCmd::Extract {
                at,
                out,
                seqs,
                wrap,
            }) => {
                let mut extraction = self.current_tree()?.plan_extract(&at, &out)?;
                let dataset = self.datasets[self.current.expect("there is a current file")].id;
                match (&seqs, wrap) {
                    (None, Some(_)) => {
                        return Err(Error::Invalid("--wrap is for --seqs".to_string()));
                    }
                    (None, None) => {}
                    (Some(seqs), wrap) => {
                        let linked = self
                            .links
                            .get(&dataset)
                            .and_then(|&id| self.datasets.iter().find(|d| d.id == id));
                        let Some((linked, alignment)) = linked.and_then(|d| Some((d, d.msa()?)))
                        else {
                            return Err(Error::Invalid(
                                "--seqs needs the tree linked to an alignment: tree link FILE"
                                    .to_string(),
                            ));
                        };
                        extraction.add_seqs(alignment, &linked.file_name(), seqs, wrap)?;
                    }
                }
                extraction.check_outputs()?;
                let label = format!("writing {}", file_name(extraction.out()));
                let work = Work::TreeExtract {
                    dataset,
                    extraction: Box::new(extraction),
                };
                return Ok(self.hand_out(label, line, work));
            }
            Command::Bam(BamCmd::Ref { file }) => {
                self.current_bam()?;
                let dataset = self.datasets[self.current.expect("there is a current file")].id;
                let label = format!("reading {}", file_name(&file));
                return Ok(self.hand_out(label, line, Work::BamRef { dataset, file }));
            }
            Command::Bam(cmd) => {
                let export = Box::new(BamExport::plan(self.current_bam()?, &cmd)?);
                let dataset = self.datasets[self.current.expect("there is a current file")].id;
                let label = format!("writing {}", file_name(export.out()));
                return Ok(self.hand_out(label, line, Work::BamExport { dataset, export }));
            }
            quick => quick,
        };

        let checkpoint = self.checkpoint(&cmd);
        let mut closed = None;
        let applied = match cmd {
            Command::Close => {
                let (applied, at, dataset) = self.close()?;
                closed = Some((at, dataset));
                applied
            }
            cmd => self.apply(cmd)?,
        };
        if let Some(log) = &mut self.log
            && let Err(error) = log.record(&line, applied.note.as_deref(), &now)
        {
            // A change that is not in the log must not stand: undo it. The
            // log is now marked incomplete, so later commands are refused.
            self.roll_back(checkpoint, closed);
            return Err(error);
        }
        self.history.push(line);
        Ok(Started::Done(Outcome {
            message: applied.message,
        }))
    }

    /// Take in the result of a [`Job`]: change the session and record the
    /// command, or, if the work failed or was cancelled, leave everything
    /// as it was.
    pub fn finish(&mut self, finished: Finished) -> Result<Outcome> {
        self.busy = None;
        let Finished { line, result } = finished;
        let now = (self.clock)();
        match result? {
            Product::Attached { dataset, reference } => {
                let Some(doc) = self.datasets.iter_mut().find(|d| d.id == dataset) else {
                    return Err(Error::Invalid(
                        "the BAM was closed while its reference was read".to_string(),
                    ));
                };
                let Content::Bam(bam) = &mut doc.content else {
                    unreachable!("bam ref is handed out for a BAM");
                };
                if let Some(log) = &mut self.log {
                    log.record(&line, None, &now)?;
                }
                let message = bam.set_reference(*reference);
                self.history.push(line);
                Ok(Outcome { message })
            }
            Product::Opened {
                input,
                content,
                message,
            } => self.record_open(&line, input, content, message, &now),
            Product::Wrote {
                dataset,
                applied,
                whole,
            } => {
                if let Some(log) = &mut self.log
                    && let Err(error) = log.record(&line, applied.note.as_deref(), &now)
                {
                    // A file that is not in the log must not stand.
                    for path in &applied.created {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(error);
                }
                let written = self.datasets.iter_mut().find(|d| d.id == dataset);
                match written.map(|d| &mut d.content) {
                    Some(Content::Msa(alignment)) if whole => alignment.mark_written(),
                    Some(Content::Tree(tree)) if whole => tree.mark_written(),
                    _ => {}
                }
                self.history.push(line);
                Ok(Outcome {
                    message: applied.message,
                })
            }
        }
    }

    /// Whether a [`Job`] has been handed out and not yet finished.
    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    pub fn datasets(&self) -> &[Dataset] {
        &self.datasets
    }

    /// The index in [`Session::datasets`] of the file that data commands
    /// apply to: the one opened last, unless `use` chose another.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The text form of every command executed so far, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn log_path(&self) -> Option<&Path> {
        self.log.as_ref().map(SessionLog::path)
    }

    /// Whether a write to the session log has failed. From then on every
    /// command is refused; [`Session::rescue`] remains.
    pub fn log_failed(&self) -> bool {
        self.log.as_ref().is_some_and(SessionLog::is_broken)
    }

    /// Write the open alignment at `dataset` (its index among the open
    /// files), whole, to a new file without logging.
    ///
    /// This exists so that work is not lost when the log can no longer be
    /// written (a full disk, a quota); it is refused while the log works.
    /// It takes the file explicitly because, with the log broken, `use N`
    /// cannot be logged and so the current file can no longer change.
    pub fn rescue(&self, dataset: usize, out: &Path) -> Result<Outcome> {
        if !self.log_failed() {
            return Err(Error::Invalid(
                "the session log has not failed; use msa extract --out PATH".to_string(),
            ));
        }
        let Some(dataset) = self.datasets.get(dataset) else {
            return Err(Error::Invalid("no file is open".to_string()));
        };
        if let Some(tree) = dataset.tree() {
            let tree = tree.tree();
            save_newick(tree, tree.root(), out, &Progress::new())?;
            return Ok(Outcome {
                message: format!(
                    "wrote {} from {} ({}); not logged, because the session log could not be written",
                    out.display(),
                    dataset.file_name(),
                    crate::msa::count(tree.tip_count(), "tip")
                ),
            });
        }
        let Some(alignment) = dataset.msa() else {
            return Err(Error::Invalid(format!(
                "{} is not an alignment",
                dataset.file_name()
            )));
        };
        let rows: Vec<usize> = (0..alignment.rows()).collect();
        write::write_fasta(alignment.view(), &rows, None, None, out, &Progress::new())?;
        Ok(Outcome {
            message: format!(
                "wrote {} from {} ({}); not logged, because the session log could not be written",
                out.display(),
                dataset.file_name(),
                alignment.shape()
            ),
        })
    }

    fn hand_out(&mut self, label: String, line: String, work: Work) -> Started {
        self.busy = Some(label.clone());
        Started::Running(Job {
            label,
            line,
            progress: Arc::new(Progress::new()),
            work: Box::new(work),
        })
    }

    /// Record an `open` and, if that succeeds, add the file and make it
    /// current.
    fn record_open(
        &mut self,
        line: &str,
        input: Input,
        content: Content,
        message: String,
        now: &Zoned,
    ) -> Result<Outcome> {
        if let Some(log) = &mut self.log {
            log.record(line, Some(&input.note), now)?;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.datasets.push(Dataset {
            id,
            path: input.path,
            kind: input.kind,
            size: input.size,
            content,
        });
        self.current = Some(self.datasets.len() - 1);
        self.history.push(line.to_string());
        Ok(Outcome { message })
    }

    fn checkpoint(&self, cmd: &Command) -> Checkpoint {
        let edits = matches!(
            cmd,
            Command::Msa(_) | Command::Tree(_) | Command::Undo | Command::Redo
        );
        let mut snapshots = Vec::new();
        if edits {
            for index in self.current.into_iter().chain(self.partner(cmd)) {
                let snapshot = match &self.datasets[index].content {
                    Content::Msa(alignment) => Snapshot::Msa(alignment.snapshot()),
                    Content::Tree(tree) => Snapshot::Tree(tree.snapshot()),
                    Content::Bam(_) | Content::Unloaded => continue,
                };
                snapshots.push((index, snapshot));
            }
        }
        Checkpoint {
            datasets: self.datasets.len(),
            current: self.current,
            snapshots,
            links: self.links.clone(),
        }
    }

    /// Put the state back to `checkpoint`, with the file the command closed
    /// back at the position it had. Undo history is not repaired: the log
    /// has failed, so no further command, `undo` included, will be accepted.
    fn roll_back(&mut self, checkpoint: Checkpoint, closed: Option<(usize, Dataset)>) {
        self.datasets.truncate(checkpoint.datasets);
        if let Some((at, dataset)) = closed {
            self.datasets.insert(at, dataset);
        }
        self.current = checkpoint.current;
        self.links = checkpoint.links;
        for (index, snapshot) in checkpoint.snapshots {
            match (snapshot, &mut self.datasets[index].content) {
                (Snapshot::Msa(layer), Content::Msa(alignment)) => alignment.restore(layer),
                (Snapshot::Tree(state), Content::Tree(tree)) => tree.restore(state),
                _ => {}
            }
        }
    }

    /// The position of the other file a command will change besides the
    /// current one: the linked file for `--seqs` and `--tree`, and for
    /// `undo` and `redo` the open file holding the other half of the step.
    fn partner(&self, cmd: &Command) -> Option<usize> {
        let current = self.current?;
        let other = |pair: Option<Pair>| self.position(pair?.with);
        match cmd {
            Command::Tree(
                TreeCmd::Prune { seqs: true, .. } | TreeCmd::Rename { seqs: true, .. },
            ) => self.position(*self.links.get(&self.datasets[current].id)?),
            Command::Msa(MsaCmd::Rename { tree: true, .. }) => {
                match self.trees_linked_to(self.datasets[current].id).as_slice() {
                    &[tree] => self.position(tree),
                    _ => None,
                }
            }
            Command::Undo => other(self.step_pair(current, true)),
            Command::Redo => other(self.step_pair(current, false)),
            _ => None,
        }
    }

    /// The position of the open file with dataset id `id`.
    fn position(&self, id: u64) -> Option<usize> {
        self.datasets.iter().position(|d| d.id == id)
    }

    /// The pair tag on the file's next undo (or redo) step.
    fn step_pair(&self, index: usize, undo: bool) -> Option<Pair> {
        match (&self.datasets[index].content, undo) {
            (Content::Msa(alignment), true) => alignment.undo_pair(),
            (Content::Msa(alignment), false) => alignment.redo_pair(),
            (Content::Tree(tree), true) => tree.undo_pair(),
            (Content::Tree(tree), false) => tree.redo_pair(),
            (Content::Bam(_) | Content::Unloaded, _) => None,
        }
    }

    /// `tree rename --seqs`: rename tips and the rows carrying the same
    /// names. The rows' renames are worked out and checked first, then the
    /// tree is renamed (the step that may refuse), then the rows.
    fn rename_from_tree(&mut self, cmd: &TreeCmd) -> Result<Applied> {
        let TreeCmd::Rename {
            at,
            to,
            regex,
            replace,
            ..
        } = cmd
        else {
            unreachable!("rename_from_tree takes a rename");
        };
        self.current_tree()?;
        let tree_at = self.current.expect("there is a current tree");
        let aln_at = self.linked_alignment()?;
        let aln_name = self.datasets[aln_at].file_name();
        let doc = self.datasets[tree_at].tree().expect("a tree");
        let alignment = self.datasets[aln_at].msa().expect("an alignment");
        // A tip whose name other tips share is renamed with them, as every
        // row carrying the name is, so no tip loses its sequence.
        let mut shared = None;
        let rows = match (to, regex, replace) {
            (Some(to), _, _) => {
                let old = doc.tip_name(at)?;
                let map = HashMap::from([(old.clone(), to.trim().to_string())]);
                let rows = alignment.renames_for(&map);
                if !old.is_empty() && doc.tips_named(&old) > 1 {
                    shared = Some((old, map));
                }
                rows
            }
            (None, Some(pattern), Some(replace)) if at.is_empty() => {
                alignment.regex_renames(pattern, replace)?
            }
            // The tree's own rename says what is wrong.
            _ => return self.current_tree()?.apply(cmd),
        };
        if let Some(name) = alignment.name_clash(&rows) {
            return Err(row_clash(&aln_name, &name));
        }
        let mut applied = match shared {
            Some((old, map)) => {
                let Content::Tree(doc) = &mut self.datasets[tree_at].content else {
                    unreachable!("the current file is a tree");
                };
                let renamed = doc
                    .rename_names("rename", &map)?
                    .expect("the tips carry the name");
                let mut applied = Applied::new(format!("{renamed} named '{old}'"));
                applied.note = Some(format!("was: {old}"));
                applied
            }
            None => self.current_tree()?.apply(cmd)?,
        };
        if rows.is_empty() {
            applied
                .message
                .push_str(&format!("; no row of {aln_name} was renamed"));
        } else {
            let renamed = rows.len();
            let Content::Msa(alignment) = &mut self.datasets[aln_at].content else {
                unreachable!("checked above");
            };
            alignment.commit_renames("rename", rows);
            self.pair_steps(tree_at, aln_at);
            applied.message.push_str(&format!(
                "; renamed {} in {aln_name}",
                count(renamed, "row")
            ));
        }
        if let Some(counts) = self.link_counts() {
            applied.message.push_str(&format!("; {}", counts.matched));
        }
        Ok(applied)
    }

    /// `msa rename --tree`: rename rows and the tips carrying their old
    /// names. The rows' renames are checked first, then the tree is renamed
    /// (the step that may refuse), then the rows.
    fn rename_from_alignment(&mut self, cmd: &MsaCmd) -> Result<Applied> {
        let MsaCmd::Rename {
            rows,
            to,
            regex,
            replace,
            ..
        } = cmd
        else {
            unreachable!("rename_from_alignment takes a rename");
        };
        self.current_alignment()?;
        let aln_at = self.current.expect("there is a current alignment");
        let aln_name = self.datasets[aln_at].file_name();
        let tree_at = match self.trees_linked_to(self.datasets[aln_at].id).as_slice() {
            [] => {
                return Err(Error::Invalid(
                    "--tree needs this alignment linked to a tree: tree link FILE from the tree's tab"
                        .to_string(),
                ));
            }
            &[tree] => self.position(tree).expect("a linked file is open"),
            trees => {
                return Err(Error::Invalid(format!(
                    "{aln_name} is linked to {} trees; --tree needs exactly one",
                    trees.len()
                )));
            }
        };
        let tree_name = self.datasets[tree_at].file_name();
        let alignment = self.datasets[aln_at].msa().expect("an alignment");
        // Every row carrying a chosen row's name is renamed with it, as
        // every tip carrying it is, so no row loses its tip.
        let map = alignment.plan_renamed_names(
            rows,
            to.as_deref(),
            regex.as_deref().zip(replace.as_deref()),
        )?;
        let plan = alignment.renames_for(&map);
        if let Some(name) = alignment.name_clash(&plan) {
            return Err(row_clash(&aln_name, &name));
        }
        let mut applied = match (plan.as_slice(), &map.iter().next()) {
            ([(row, new)], Some((old, _))) => {
                let mut applied = Applied::new(format!("renamed row {} to '{new}'", row + 1));
                applied.note = Some(format!("was: {old}"));
                applied
            }
            _ => Applied::new(format!("renamed {}", count(plan.len(), "row"))),
        };
        let Content::Tree(doc) = &mut self.datasets[tree_at].content else {
            unreachable!("a linked tree is a tree");
        };
        let tips = doc.rename_names("rename", &map)?;
        let Content::Msa(alignment) = &mut self.datasets[aln_at].content else {
            unreachable!("the current file is an alignment");
        };
        alignment.commit_renames("rename", plan);
        match tips {
            Some(renamed) => {
                self.pair_steps(aln_at, tree_at);
                applied
                    .message
                    .push_str(&format!("; {renamed} in {tree_name}"));
            }
            None => applied
                .message
                .push_str(&format!("; no tip of {tree_name} was renamed")),
        }
        Ok(applied)
    }

    /// `undo` or `redo` in the current file. A step that was half of an
    /// edit to two files is undone (or redone) in both, or refused, unless
    /// the other file has been closed.
    fn undo_or_redo(&mut self, undo: bool) -> Result<Applied> {
        let Some(index) = self.current else {
            return Err(Error::Invalid("no file is open".to_string()));
        };
        if matches!(self.datasets[index].content, Content::Bam(_)) {
            let what = if undo { "undo" } else { "redo" };
            return Err(Error::Invalid(format!("a BAM has no edits to {what}")));
        }
        let Some(pair) = self.step_pair(index, undo) else {
            return self.step(undo);
        };
        let Some(other) = self.position(pair.with) else {
            let mut applied = self.step(undo)?;
            applied
                .message
                .push_str("; the other file is closed, so only this one changed");
            return Ok(applied);
        };
        let name = self.datasets[other].file_name();
        if self.step_pair(other, undo).map(|p| p.id) != Some(pair.id) {
            let keeps = match (&self.datasets[other].content, undo) {
                (Content::Msa(alignment), true) => alignment.keeps(pair.id),
                (Content::Msa(alignment), false) => alignment.redo_keeps(pair.id),
                (Content::Tree(tree), true) => tree.keeps(pair.id),
                (Content::Tree(tree), false) => tree.redo_keeps(pair.id),
                (Content::Bam(_) | Content::Unloaded, _) => false,
            };
            return Err(Error::Invalid(match (undo, keeps) {
                (true, true) => format!("undo the later edit in {name} first"),
                (true, false) => {
                    format!("{name} no longer keeps that step, so it cannot be undone in both")
                }
                (false, true) => format!("redo in {name} first"),
                (false, false) => {
                    format!("{name} has been edited since, so this cannot be redone in both")
                }
            }));
        }
        let mut applied = self.step(undo)?;
        let theirs = match &mut self.datasets[other].content {
            Content::Msa(alignment) if undo => alignment.undo(),
            Content::Msa(alignment) => alignment.redo(),
            Content::Tree(tree) if undo => tree.undo(),
            Content::Tree(tree) => tree.redo(),
            Content::Bam(_) | Content::Unloaded => {
                unreachable!("a file with an undo step has edits")
            }
        }?;
        applied.message = format!("{}; and in {name}: {}", applied.message, theirs.message);
        Ok(applied)
    }

    /// Undo or redo one step of the current file alone.
    fn step(&mut self, undo: bool) -> Result<Applied> {
        match (self.current_is_tree(), undo) {
            (true, true) => self.current_tree()?.undo(),
            (true, false) => self.current_tree()?.redo(),
            (false, true) => self.current_alignment()?.undo(),
            (false, false) => self.current_alignment()?.redo(),
        }
    }

    /// Tag the newest steps of the files at `a` and `b` as one edit.
    fn pair_steps(&mut self, a: usize, b: usize) {
        let id = self.next_pair;
        self.next_pair += 1;
        for (index, other) in [(a, b), (b, a)] {
            let pair = Pair {
                id,
                with: self.datasets[other].id,
            };
            match &mut self.datasets[index].content {
                Content::Msa(alignment) => alignment.pair_last(pair),
                Content::Tree(tree) => tree.pair_last(pair),
                Content::Bam(_) | Content::Unloaded => {}
            }
        }
    }

    /// The dataset ids of the trees linked to the alignment `alignment`.
    pub fn trees_linked_to(&self, alignment: u64) -> Vec<u64> {
        let mut trees: Vec<u64> = self
            .links
            .iter()
            .filter(|&(_, &aln)| aln == alignment)
            .map(|(&tree, _)| tree)
            .collect();
        trees.sort_unstable();
        trees
    }

    /// The position of the alignment linked to the current tree, for
    /// `--seqs`.
    fn linked_alignment(&self) -> Result<usize> {
        self.current
            .and_then(|index| self.links.get(&self.datasets[index].id))
            .and_then(|&id| self.position(id))
            .filter(|&index| self.datasets[index].msa().is_some())
            .ok_or_else(|| {
                Error::Invalid(
                    "--seqs needs the tree linked to an alignment: tree link FILE".to_string(),
                )
            })
    }

    /// `tree prune --seqs`: prune the clade and delete its tips' rows from
    /// the linked alignment. Everything that could refuse is checked before
    /// the prune, the one step that may still fail, so a refusal changes
    /// neither file.
    fn prune_linked(&mut self, cmd: &TreeCmd) -> Result<Applied> {
        let TreeCmd::Prune { at, .. } = cmd else {
            unreachable!("prune_linked takes a prune");
        };
        self.current_tree()?;
        let tree_at = self.current.expect("there is a current tree");
        let aln_at = self.linked_alignment()?;
        if at.is_empty() {
            // The prune itself says what is missing.
            return self.current_tree()?.apply(cmd);
        }
        let aln_name = self.datasets[aln_at].file_name();
        let names = self.datasets[tree_at]
            .tree()
            .expect("a tree")
            .clade_tip_names(at)?;
        let alignment = self.datasets[aln_at].msa().expect("an alignment");
        let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
        let rows = alignment.rows_named(&wanted);
        if !rows.is_empty() && rows.len() == alignment.rows() {
            return Err(Error::Invalid(format!(
                "{aln_name}: that would delete every sequence"
            )));
        }
        let found: HashSet<&str> = rows.iter().map(|&row| alignment.name(row)).collect();
        let without = names
            .iter()
            .filter(|name| !found.contains(name.as_str()))
            .count();
        let mut applied = self.current_tree()?.apply(cmd)?;
        if rows.is_empty() {
            applied
                .message
                .push_str(&format!("; no row of {aln_name} carried those names"));
        } else {
            let Content::Msa(alignment) = &mut self.datasets[aln_at].content else {
                unreachable!("checked above");
            };
            alignment.delete_listed("prune", &rows);
            self.pair_steps(tree_at, aln_at);
            applied.message.push_str(&format!(
                "; deleted {} from {aln_name}",
                count(rows.len(), "row")
            ));
            if without > 0 {
                let had = if without == 1 { "tip had" } else { "tips had" };
                applied
                    .message
                    .push_str(&format!(" ({without} {had} no sequence)"));
            }
        }
        if let Some(counts) = self.link_counts() {
            applied.message.push_str(&format!("; {}", counts.matched));
        }
        Ok(applied)
    }

    /// The alignment that data commands apply to.
    fn current_alignment(&mut self) -> Result<&mut Alignment> {
        let Some(index) = self.current else {
            return Err(Error::Invalid("no file is open".to_string()));
        };
        let dataset = &mut self.datasets[index];
        let name = dataset.file_name();
        match &mut dataset.content {
            Content::Msa(alignment) => Ok(alignment),
            Content::Tree(_) | Content::Bam(_) | Content::Unloaded => Err(Error::Invalid(format!(
                "{name} is not an alignment; switch with use N"
            ))),
        }
    }

    /// The alignment linked to the tree `tree` (dataset ids).
    pub fn link_of(&self, tree: u64) -> Option<u64> {
        self.links.get(&tree).copied()
    }

    /// How the tree with dataset id `tree` matches its linked alignment;
    /// `None` when it is not linked.
    pub fn link_summary(&self, tree: u64) -> Option<LinkSummary> {
        let alignment = *self.links.get(&tree)?;
        let doc = self.datasets.iter().find(|d| d.id == tree)?.tree()?;
        let msa = self.datasets.iter().find(|d| d.id == alignment)?.msa()?;
        let names: std::collections::HashSet<&str> =
            (0..msa.rows()).map(|row| msa.name(row)).collect();
        let tree = doc.tree();
        let matched = tree
            .tips()
            .into_iter()
            .filter(|&n| names.contains(tree.label(n).unwrap_or_default()))
            .count();
        Some(LinkSummary {
            alignment,
            matched,
            tips: doc.tips(),
        })
    }

    /// How the current tree's tips match its linked alignment's names.
    fn link_counts(&self) -> Option<LinkCounts> {
        let tree_set = &self.datasets[self.current?];
        let tree = tree_set.tree()?.tree();
        let aln_id = self.links.get(&tree_set.id)?;
        let alignment = self.datasets.iter().find(|d| d.id == *aln_id)?.msa()?;
        let names: std::collections::HashSet<&str> = (0..alignment.rows())
            .map(|row| alignment.name(row))
            .collect();
        let tips = tree.tips();
        let tip_names: std::collections::HashSet<&str> =
            tips.iter().filter_map(|&n| tree.label(n)).collect();
        let without: Vec<&str> = tips
            .iter()
            .map(|&n| tree.label(n).unwrap_or_default())
            .filter(|name| !names.contains(name))
            .collect();
        let no_tip: Vec<&str> = (0..alignment.rows())
            .map(|row| alignment.name(row))
            .filter(|name| !tip_names.contains(name))
            .collect();
        let have = if tips.len() == 1 {
            "tip has"
        } else {
            "tips have"
        };
        let mut unmatched = String::new();
        if !without.is_empty() {
            unmatched.push_str(&format!("; tips without one: {}", some_names(&without)));
        }
        if !no_tip.is_empty() {
            let has = if no_tip.len() == 1 { "has" } else { "have" };
            unmatched.push_str(&format!(
                "; {} {has} no tip: {}",
                count(no_tip.len(), "sequence"),
                some_names(&no_tip)
            ));
        }
        Some(LinkCounts {
            matched: format!(
                "{} of {} {have} a sequence",
                tips.len() - without.len(),
                tips.len()
            ),
            unmatched,
        })
    }

    /// `tree link FILE`: pair the current tree with an open alignment.
    fn link(&mut self, file: &Path) -> Result<Applied> {
        self.current_tree()?;
        let index = self.current.expect("there is a current tree");
        let same = |path: &Path| {
            path == file
                || matches!(
                    (std::fs::canonicalize(path), std::fs::canonicalize(file)),
                    (Ok(a), Ok(b)) if a == b
                )
        };
        let Some(target) = self.datasets.iter().find(|d| same(&d.path)) else {
            return Err(Error::Invalid(format!(
                "{} is not open; open it first",
                file.display()
            )));
        };
        if target.msa().is_none() {
            return Err(Error::Invalid(format!(
                "{} is not an alignment",
                target.file_name()
            )));
        }
        let (tree_id, aln_id) = (self.datasets[index].id, target.id);
        let message = format!(
            "linked {} to {}",
            self.datasets[index].file_name(),
            target.file_name()
        );
        self.links.insert(tree_id, aln_id);
        let counts = self.link_counts().expect("just linked");
        Ok(Applied {
            message: format!("{message}: {}{}", counts.matched, counts.unmatched),
            note: None,
            created: Vec::new(),
        })
    }

    /// `tree unlink`.
    fn unlink(&mut self) -> Result<Applied> {
        self.current_tree()?;
        let tree = &self.datasets[self.current.expect("there is a current tree")];
        let Some(aln_id) = self.links.remove(&tree.id) else {
            return Err(Error::Invalid(format!(
                "{} is not linked",
                tree.file_name()
            )));
        };
        let alignment = self.datasets.iter().find(|d| d.id == aln_id);
        Ok(Applied {
            message: format!(
                "unlinked {} from {}",
                tree.file_name(),
                alignment.map(Dataset::file_name).unwrap_or_default()
            ),
            note: None,
            created: Vec::new(),
        })
    }

    fn current_is_tree(&self) -> bool {
        self.current
            .is_some_and(|index| self.datasets[index].tree().is_some())
    }

    /// The tree that tree commands apply to.
    /// Write `names` to a new names file beside the log, for a command's
    /// `--names-file`; `None` when the session keeps no log.
    pub fn names_file(&mut self, names: &[String]) -> Result<Option<PathBuf>> {
        match &mut self.log {
            Some(log) => log.write_names(names).map(Some),
            None => Ok(None),
        }
    }

    fn current_bam(&mut self) -> Result<&mut BamDoc> {
        let Some(index) = self.current else {
            return Err(Error::Invalid("no file is open".to_string()));
        };
        let dataset = &mut self.datasets[index];
        let name = dataset.file_name();
        match &mut dataset.content {
            Content::Bam(bam) => Ok(bam),
            Content::Msa(_) | Content::Tree(_) | Content::Unloaded => Err(Error::Invalid(format!(
                "{name} is not a BAM; switch with use N"
            ))),
        }
    }

    fn current_tree(&mut self) -> Result<&mut TreeDoc> {
        let Some(index) = self.current else {
            return Err(Error::Invalid("no file is open".to_string()));
        };
        let dataset = &mut self.datasets[index];
        let name = dataset.file_name();
        match &mut dataset.content {
            Content::Tree(tree) => Ok(tree),
            Content::Msa(_) | Content::Bam(_) | Content::Unloaded => Err(Error::Invalid(format!(
                "{name} is not a tree; switch with use N"
            ))),
        }
    }

    /// Close the current file. Returns it with the position it had, so that
    /// it can be put back if the command cannot be logged.
    fn close(&mut self) -> Result<(Applied, usize, Dataset)> {
        let Some(at) = self.current else {
            return Err(Error::Invalid("no file is open".to_string()));
        };
        let dataset = self.datasets.remove(at);
        // The file that moved into this position, else the last one.
        self.current = match self.datasets.len() {
            0 => None,
            open => Some(at.min(open - 1)),
        };
        let mut message = format!("closed {}", dataset.file_name());
        if let Some(index) = self.current {
            message.push_str(&format!(
                "; using {}: {}",
                index + 1,
                self.datasets[index].file_name()
            ));
        }
        let name_of = |id: u64| {
            let open = self.datasets.iter().chain(std::iter::once(&dataset));
            open.into_iter()
                .find(|d| d.id == id)
                .map(Dataset::file_name)
                .unwrap_or_default()
        };
        let dropped: Vec<(u64, u64)> = self
            .links
            .iter()
            .filter(|&(&tree, &aln)| tree == dataset.id || aln == dataset.id)
            .map(|(&tree, &aln)| (tree, aln))
            .collect();
        for (tree, aln) in dropped {
            self.links.remove(&tree);
            message.push_str(&format!(
                "; the link between {} and {} is dropped",
                name_of(tree),
                name_of(aln)
            ));
        }
        let applied = Applied {
            message,
            note: None,
            created: Vec::new(),
        };
        Ok((applied, at, dataset))
    }

    /// Apply a quick command to the session state.
    fn apply(&mut self, cmd: Command) -> Result<Applied> {
        match cmd {
            Command::Open { .. } | Command::Close => {
                unreachable!("start handles opening and closing itself")
            }
            Command::Bam(_) => unreachable!("start hands every bam command out as a job"),
            Command::Use { dataset } => {
                let open = self.datasets.len();
                if open == 0 {
                    return Err(Error::Invalid("no file is open".to_string()));
                }
                if dataset == 0 || dataset > open {
                    let verb = if open == 1 { "is" } else { "are" };
                    return Err(Error::Invalid(format!(
                        "there is no file {dataset} ({open} {verb} open)"
                    )));
                }
                self.current = Some(dataset - 1);
                Ok(Applied {
                    message: format!(
                        "using {dataset}: {}",
                        self.datasets[dataset - 1].file_name()
                    ),
                    note: None,
                    created: Vec::new(),
                })
            }
            Command::Undo => self.undo_or_redo(true),
            Command::Redo => self.undo_or_redo(false),
            Command::Msa(cmd @ MsaCmd::Rename { tree: true, .. }) => {
                self.rename_from_alignment(&cmd)
            }
            Command::Msa(cmd) => self.current_alignment()?.apply(&cmd),
            Command::Tree(TreeCmd::Link { file }) => self.link(&file),
            Command::Tree(TreeCmd::Unlink) => self.unlink(),
            Command::Tree(cmd @ TreeCmd::Prune { seqs: true, .. }) => self.prune_linked(&cmd),
            Command::Tree(cmd @ TreeCmd::Rename { seqs: true, .. }) => self.rename_from_tree(&cmd),
            Command::Tree(cmd) => {
                let mut applied = self.current_tree()?.apply(&cmd)?;
                if matches!(cmd, TreeCmd::Rename { .. })
                    && let Some(counts) = self.link_counts()
                {
                    applied.message.push_str(&format!("; {}", counts.matched));
                }
                Ok(applied)
            }
        }
    }
}

/// A linked rename refused because it would give one name to several rows.
fn row_clash(alignment: &str, name: &str) -> Error {
    Error::Invalid(format!(
        "{alignment}: that would give '{name}' to more than one sequence"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::tests::{fixed, limited_log};
    use crate::{Command, Error, Kind, SessionLog};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// A temp dir holding a 4-byte `aln.fa`.
    fn setup() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let fa = dir.path().join("aln.fa");
        fs::write(&fa, ">a\nA").unwrap();
        (dir, fa)
    }

    fn open(path: &Path) -> Command {
        Command::Open {
            path: path.to_path_buf(),
            kind: None,
        }
    }

    #[test]
    fn open_registers_and_logs() {
        let (dir, fa) = setup();
        let log = dir.path().join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        let out = s.execute(open(&fa)).unwrap();
        assert!(
            out.message.ends_with("(msa, 1 sequence x 1 column)"),
            "{}",
            out.message
        );
        assert_eq!(s.datasets().len(), 1);
        assert_eq!(s.datasets()[0].kind, Kind::Msa);
        assert_eq!(s.datasets()[0].size, 4);
        assert_eq!(s.history(), [format!("open {}", fa.display())]);
        let text = fs::read_to_string(&log).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], "# seqpeeq 0.1.0");
        assert_eq!(lines[1], "# started 2026-11-15T16:05:03-08:00");
        assert!(lines[2].starts_with("# cwd /"), "{}", lines[2]);
        assert_eq!(lines[3], "# 2026-11-15T16:05:03-08:00");
        assert_eq!(lines[4], format!("open {}", fa.display()));
        assert!(
            lines[5].starts_with("# input /") && lines[5].contains(" size=4 mtime="),
            "{}",
            lines[5]
        );
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn log_is_lazy() {
        let (dir, _fa) = setup();
        let log = dir.path().join("s.log");
        let s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        assert_eq!(s.log_path(), Some(log.as_path()));
        assert!(!log.exists());
    }

    #[test]
    fn failed_command_is_not_logged() {
        let (dir, _fa) = setup();
        let log = dir.path().join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        let err = s.execute(open(&dir.path().join("missing.fa"))).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert!(s.datasets().is_empty());
        assert!(s.history().is_empty());
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 3);
    }

    #[test]
    fn log_with_no_commands_is_removed_when_session_ends() {
        let (dir, _fa) = setup();
        let log = dir.path().join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        s.execute(open(&dir.path().join("missing.fa"))).unwrap_err();
        assert!(log.exists());
        drop(s);
        assert!(!log.exists());
    }

    #[test]
    fn log_with_commands_survives_session_end() {
        let (dir, fa) = setup();
        let log = dir.path().join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        s.execute(open(&fa)).unwrap();
        drop(s);
        assert!(log.exists());
    }

    #[test]
    fn log_write_failure_undoes_the_command_and_refuses_later_ones() {
        let (_dir, fa) = setup();
        // Room for the header but not for a command record.
        let (log, buffer) = limited_log(160);
        let mut s = Session::with_clock(Some(log), fixed);
        let err = s.execute(open(&fa)).unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert!(s.datasets().is_empty(), "the open must be rolled back");
        assert!(s.history().is_empty());
        let after_first = buffer.lock().unwrap().clone();
        assert!(
            !String::from_utf8(after_first.clone())
                .unwrap()
                .contains("open ")
        );

        let err = s.execute(open(&fa)).unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert!(s.datasets().is_empty());
        assert_eq!(*buffer.lock().unwrap(), after_first);
    }

    #[cfg(unix)]
    #[test]
    fn line_break_in_a_resolved_path_does_not_corrupt_the_log() {
        let (dir, _fa) = setup();
        let odd = dir.path().join("a\nopen evil.fa");
        fs::write(&odd, ">a\nA").unwrap();
        let link = dir.path().join("link.fa");
        std::os::unix::fs::symlink(&odd, &link).unwrap();
        let log = dir.path().join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        s.execute(open(&link)).unwrap();
        let text = fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 6, "{text}");

        let mut again = Session::new(None);
        let report = crate::replay(&text, &mut again).unwrap();
        assert_eq!((report.commands, again.datasets().len()), (1, 1));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn unknown_kind_is_refused() {
        let (dir, _fa) = setup();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "(a,b);").unwrap();
        let mut s = Session::new(None);
        let err = s.execute(open(&notes)).unwrap_err();
        assert!(matches!(err, Error::UnknownKind(_)), "{err}");
        s.execute(Command::Open {
            path: notes,
            kind: Some(Kind::Tree),
        })
        .unwrap();
        assert_eq!(s.datasets()[0].kind, Kind::Tree);
    }

    #[test]
    fn unwritable_log_refuses_command() {
        let (dir, fa) = setup();
        let log = dir.path().join("no-such-dir").join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log)), fixed);
        let err = s.execute(open(&fa)).unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        let text = err.to_string();
        assert!(
            text.contains("--log PATH") && text.contains("--no-log"),
            "{text}"
        );
        assert!(s.datasets().is_empty());
        assert!(s.history().is_empty());
    }

    #[test]
    fn existing_log_is_never_overwritten() {
        let (dir, fa) = setup();
        let log = dir.path().join("s.log");
        fs::write(&log, "keep").unwrap();
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        let err = s.execute(open(&fa)).unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert_eq!(fs::read_to_string(&log).unwrap(), "keep");
        assert!(s.datasets().is_empty());
    }

    #[test]
    fn no_log_session_works() {
        let (_dir, fa) = setup();
        let mut s = Session::new(None);
        s.execute(open(&fa)).unwrap();
        assert_eq!(s.datasets().len(), 1);
        assert_eq!(s.log_path(), None);
    }

    use crate::msa::testing::TRIO;

    /// A temp dir holding `a.fa` and `b.fa` (both TRIO) and `t.nwk`.
    fn trio_dir() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("a.fa"), TRIO).unwrap();
        fs::write(path.join("b.fa"), TRIO).unwrap();
        fs::write(path.join("t.nwk"), "(a,b);").unwrap();
        (dir, path)
    }

    fn ex(session: &mut Session, line: &str) -> crate::Result<Outcome> {
        session.execute(Command::parse_line(line).unwrap())
    }

    /// A session with no log and the named files of `dir` opened in order.
    fn opened(dir: &Path, names: &[&str]) -> Session {
        let mut s = Session::new(None);
        for name in names {
            s.execute(open(&dir.join(name))).unwrap();
        }
        s
    }

    fn cols(session: &Session, dataset: usize) -> usize {
        session.datasets()[dataset].msa().unwrap().cols()
    }

    #[test]
    fn open_loads_the_alignment_and_makes_it_current() {
        let (_keep, dir) = trio_dir();
        let mut s = Session::new(None);
        let out = s.execute(open(&dir.join("a.fa"))).unwrap();
        assert_eq!(
            out.message,
            format!(
                "opened {} (msa, 3 sequences x 10 columns)",
                dir.join("a.fa").display()
            )
        );
        assert_eq!(s.datasets()[0].msa().unwrap().rows(), 3);
        assert_eq!(s.current(), Some(0));
        let out = s.execute(open(&dir.join("t.nwk"))).unwrap();
        assert!(out.message.ends_with("(tree, 2 tips)"), "{}", out.message);
        assert!(s.datasets()[1].msa().is_none());
        assert_eq!(s.current(), Some(1));
    }

    #[test]
    fn a_file_that_is_not_fasta_is_not_opened() {
        let (_keep, dir) = trio_dir();
        fs::write(dir.join("bad.fa"), "hello\n").unwrap();
        let log = dir.join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        let err = s.execute(open(&dir.join("bad.fa"))).unwrap_err();
        assert!(
            err.to_string()
                .ends_with("bad.fa: line 1 does not start with '>': not a FASTA file"),
            "{err}"
        );
        assert!(s.datasets().is_empty() && s.history().is_empty());
        assert_eq!(s.current(), None);
        drop(s);
        assert!(!log.exists());
    }

    #[test]
    fn use_switches_the_current_file() {
        let (_keep, dir) = trio_dir();
        let mut s = opened(&dir, &["a.fa", "b.fa"]);
        assert_eq!(s.current(), Some(1));
        assert_eq!(ex(&mut s, "use 1").unwrap().message, "using 1: a.fa");
        assert_eq!(s.current(), Some(0));
        for (line, message) in [
            ("use 3", "there is no file 3 (2 are open)"),
            ("use 0", "there is no file 0 (2 are open)"),
        ] {
            assert_eq!(ex(&mut s, line).unwrap_err().to_string(), message);
            assert_eq!(s.current(), Some(0));
        }
        assert_eq!(
            ex(&mut opened(&dir, &["a.fa"]), "use 2")
                .unwrap_err()
                .to_string(),
            "there is no file 2 (1 is open)"
        );
        assert_eq!(
            ex(&mut Session::new(None), "use 1")
                .unwrap_err()
                .to_string(),
            "no file is open"
        );
    }

    #[test]
    fn commands_edit_only_the_current_file() {
        let (_keep, dir) = trio_dir();
        let mut s = opened(&dir, &["a.fa", "b.fa"]);
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "msa delete-cols --cols 1-3").unwrap();
        assert_eq!((cols(&s, 0), cols(&s, 1)), (7, 10));
        assert_eq!(
            ex(&mut s, "undo").unwrap().message,
            "undid delete-cols; now 3 sequences x 10 columns"
        );
        ex(&mut s, "redo").unwrap();
        assert_eq!(cols(&s, 0), 7);
        ex(&mut s, "use 2").unwrap();
        assert_eq!(
            ex(&mut s, "undo").unwrap_err().to_string(),
            "nothing to undo"
        );
    }

    #[test]
    fn data_commands_need_an_alignment() {
        let (_keep, dir) = trio_dir();
        let mut s = Session::new(None);
        for line in ["msa delete-cols --cols 1", "undo", "redo"] {
            assert_eq!(ex(&mut s, line).unwrap_err().to_string(), "no file is open");
        }
        let mut s = opened(&dir, &["a.fa", "t.nwk"]);
        assert_eq!(
            ex(&mut s, "msa delete-cols --cols 1")
                .unwrap_err()
                .to_string(),
            "t.nwk is not an alignment; switch with use N"
        );
        assert_eq!(cols(&s, 0), 10);
    }

    /// A temp dir holding `t.nwk` = `((A:1,B:2)90:3,C:4);`, `dup.nwk` =
    /// `((A,A),B);` and `a.fa` with sequences A, B, C and Z.
    fn trees() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("t.nwk"), "((A:1,B:2)90:3,C:4);").unwrap();
        fs::write(path.join("dup.nwk"), "((A,A),B);").unwrap();
        fs::write(path.join("l.nwk"), "((A,B),(C,D));").unwrap();
        fs::write(path.join("two.fa"), ">A\nAC\n>B\nGG\n>C\nTT\n>A\nCC\n").unwrap();
        fs::write(
            path.join("a.fa"),
            ">A\nAC-T\n>B\nACGT\n>C\nTTTT\n>Z\nGGGG\n",
        )
        .unwrap();
        (dir, path)
    }

    /// Run a command line with `{}` replaced by the directory.
    fn ex_in(session: &mut Session, dir: &Path, line: &str) -> crate::Result<Outcome> {
        let dir = shlex::try_quote(dir.to_str().unwrap())
            .unwrap()
            .into_owned();
        ex(session, &line.replace("{}", &dir))
    }

    #[test]
    fn a_tree_opens_and_says_what_it_holds() {
        let (_keep, dir) = trees();
        let mut s = Session::new(None);
        let out = s.execute(open(&dir.join("t.nwk"))).unwrap();
        assert_eq!(
            out.message,
            format!("opened {} (tree, 3 tips)", dir.join("t.nwk").display())
        );
        assert_eq!(s.datasets()[0].tree().unwrap().tips(), 3);
        assert!(s.datasets()[0].msa().is_none() && !s.datasets()[0].is_modified());
        let out = s.execute(open(&dir.join("dup.nwk"))).unwrap();
        assert!(
            out.message.ends_with(
                "(tree, 3 tips); 1 name is shared by more than one tip; name those tips with --node"
            ),
            "{}",
            out.message
        );
        fs::write(dir.join("two.nwk"), "[c](x,y);(p,q);").unwrap();
        let out = s.execute(open(&dir.join("two.nwk"))).unwrap();
        assert!(
            out.message.ends_with(
                "(tree, 2 tips); skipped 1 more tree in the file; comments were dropped"
            ),
            "{}",
            out.message
        );
        fs::write(dir.join("bad.nwk"), "(x,y").unwrap();
        let error = s
            .execute(open(&dir.join("bad.nwk")))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "{}: line 1, column 5: missing ')'",
                dir.join("bad.nwk").display()
            )
        );
        assert_eq!(s.datasets().len(), 3);
    }

    #[test]
    fn a_clade_or_the_whole_tree_is_extracted() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["t.nwk"]);
        let out = ex_in(
            &mut s,
            &dir,
            "tree extract --mrca A --mrca B --out {}/ab.nwk",
        )
        .unwrap();
        assert_eq!(
            out.message,
            format!("wrote {} (2 tips)", dir.join("ab.nwk").display())
        );
        assert_eq!(
            fs::read_to_string(dir.join("ab.nwk")).unwrap(),
            "(A:1,B:2)90;\n"
        );
        ex_in(&mut s, &dir, "tree save -o {}/all.nwk").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("all.nwk")).unwrap(),
            "((A:1,B:2)90:3,C:4);\n"
        );
        assert_eq!(
            ex_in(&mut s, &dir, "tree extract --out {}/ab.nwk")
                .unwrap_err()
                .to_string(),
            format!(
                "{} already exists; choose another name or remove it first",
                dir.join("ab.nwk").display()
            )
        );
        assert!(!s.datasets()[0].is_modified());
        assert_eq!(
            s.history()[1..],
            [
                format!(
                    "tree extract --mrca A --mrca B --out {}/ab.nwk",
                    dir.display()
                ),
                format!("tree extract --out {}/all.nwk", dir.display())
            ]
        );
    }

    #[test]
    fn nodes_are_named_by_tip_pair_or_number_and_shared_names_are_refused() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["dup.nwk"]);
        let err = |s: &mut Session, line| ex_in(s, &dir, line).unwrap_err().to_string();
        assert_eq!(
            err(&mut s, "tree extract --tip A --out {}/x.nwk"),
            "'A' names 2 tips; name one with --node N (the tree view writes it for you)"
        );
        assert_eq!(
            err(&mut s, "tree extract --mrca A --mrca B --out {}/x.nwk"),
            "'A' names 2 tips; name one with --node N (the tree view writes it for you)"
        );
        ex_in(&mut s, &dir, "tree extract --node 2 --out {}/x.nwk").unwrap();
        assert_eq!(fs::read_to_string(dir.join("x.nwk")).unwrap(), "(A,A);\n");
        assert_eq!(
            err(&mut s, "tree extract --node 9 --out {}/y.nwk"),
            "there is no node 9 (the tree has 5)"
        );
        assert_eq!(
            err(&mut s, "tree extract --node 0 --out {}/y.nwk"),
            "there is no node 0 (the tree has 5)"
        );
        assert_eq!(
            err(&mut s, "tree extract --mrca B --out {}/y.nwk"),
            "--mrca takes two tips"
        );
        assert_eq!(
            err(&mut s, "tree extract --tip Q --out {}/y.nwk"),
            "no tip is named 'Q'"
        );
        let mut s = opened(&dir, &["t.nwk"]);
        ex_in(
            &mut s,
            &dir,
            "tree extract --mrca B --mrca B --out {}/b.nwk",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.nwk")).unwrap(), "B;\n");
        ex_in(
            &mut s,
            &dir,
            "tree extract --mrca C --mrca A --out {}/r.nwk",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("r.nwk")).unwrap(),
            "((A:1,B:2)90:3,C:4);\n"
        );
        assert!(!dir.join("y.nwk").exists());
    }

    #[test]
    fn link_summary_counts_tips_with_a_sequence() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "t.nwk"]);
        let tree = s.datasets()[1].id;
        assert_eq!(s.link_summary(tree), None);
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        let got = s.link_summary(tree).unwrap();
        assert_eq!(
            (got.alignment, got.matched, got.tips),
            (s.datasets()[0].id, 3, 3)
        );
    }

    #[test]
    fn a_linked_clade_is_written_with_its_sequences_in_tree_order() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "t.nwk"]);
        assert_eq!(
            ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap().message,
            "linked t.nwk to a.fa: 3 of 3 tips have a sequence; 1 sequence has no tip: Z"
        );
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "msa fill --name A --cols 1 --with N").unwrap();
        ex(&mut s, "use 2").unwrap();
        ex(&mut s, "tree reroot --tip B").unwrap();
        let out = ex_in(&mut s, &dir, "tree extract --out {}/s.nwk --seqs {}/s.fa").unwrap();
        assert_eq!(
            out.message,
            format!(
                "wrote {} (3 tips) and {} (3 sequences)",
                dir.join("s.nwk").display(),
                dir.join("s.fa").display()
            )
        );
        assert_eq!(
            fs::read_to_string(dir.join("s.nwk")).unwrap(),
            "(B:1,(A:1,C:7):1);\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("s.fa")).unwrap(),
            ">B\nACGT\n>A\nNC-T\n>C\nTTTT\n",
            "tree order, the alignment as it now is"
        );
        ex_in(
            &mut s,
            &dir,
            "tree extract --tip C --out {}/c.nwk --seqs {}/c.fa --wrap 3",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("c.fa")).unwrap(),
            ">C\nTTT\nT\n"
        );
        assert!(!s.datasets()[1].is_modified(), "the whole tree was written");
    }

    #[test]
    fn missing_and_repeated_names_are_reported() {
        let (_keep, dir) = trees();
        fs::write(dir.join("d.fa"), ">A\nAAAA\n>B\nCCCC\n>B\nGGGG\n").unwrap();
        let mut s = opened(&dir, &["d.fa", "t.nwk"]);
        assert_eq!(
            ex_in(&mut s, &dir, "tree link {}/d.fa").unwrap().message,
            "linked t.nwk to d.fa: 2 of 3 tips have a sequence; tips without one: C"
        );
        let out = ex_in(&mut s, &dir, "tree extract --out {}/s.nwk --seqs {}/s.fa").unwrap();
        assert!(
            out.message.ends_with(
                "(2 sequences); 1 tip has no sequence: C; 1 name is in the alignment more than once; its first row was used"
            ),
            "{}",
            out.message
        );
        assert_eq!(
            fs::read_to_string(dir.join("s.fa")).unwrap(),
            ">A\nAAAA\n>B\nCCCC\n"
        );
        assert_eq!(
            ex_in(
                &mut s,
                &dir,
                "tree extract --tip C --out {}/x.nwk --seqs {}/x.fa"
            )
            .unwrap_err()
            .to_string(),
            "no tip in the clade has a sequence in d.fa"
        );
        assert!(!dir.join("x.nwk").exists());
    }

    #[test]
    fn nothing_is_written_when_either_file_exists_or_the_link_is_gone() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "t.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        fs::write(dir.join("taken.fa"), "").unwrap();
        let err = |s: &mut Session, line| ex_in(s, &dir, line).unwrap_err().to_string();
        assert_eq!(
            err(&mut s, "tree extract --out {}/new.nwk --seqs {}/taken.fa"),
            format!(
                "{} already exists; choose another name or remove it first",
                dir.join("taken.fa").display()
            )
        );
        assert_eq!(
            err(&mut s, "tree extract --out {}/new.nwk --seqs {}/new.nwk"),
            "the tree and its sequences need two different files"
        );
        assert_eq!(
            err(
                &mut s,
                "tree extract --out {}/new.nwk --seqs {}/n.fa --wrap 0"
            ),
            "--wrap must be at least 1"
        );
        assert!(!dir.join("new.nwk").exists());
        ex(&mut s, "use 1").unwrap();
        assert!(
            ex(&mut s, "close")
                .unwrap()
                .message
                .ends_with("; the link between t.nwk and a.fa is dropped")
        );
        assert_eq!(
            err(&mut s, "tree extract --out {}/new.nwk --seqs {}/n.fa"),
            "--seqs needs the tree linked to an alignment: tree link FILE"
        );
        assert_eq!(
            err(&mut s, "tree extract --out {}/new.nwk --wrap 60"),
            "--wrap is for --seqs"
        );
        assert!(!dir.join("new.nwk").exists());
    }

    #[test]
    fn a_link_is_not_an_edit_and_counts_follow_renames() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "t.nwk"]);
        assert_eq!(
            ex_in(&mut s, &dir, "tree link {}/b.fa")
                .unwrap_err()
                .to_string(),
            format!("{} is not open; open it first", dir.join("b.fa").display())
        );
        assert_eq!(
            ex_in(&mut s, &dir, "tree link {}/t.nwk")
                .unwrap_err()
                .to_string(),
            "t.nwk is not an alignment"
        );
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        assert_eq!(s.link_of(s.datasets()[1].id), Some(s.datasets()[0].id));
        assert_eq!(
            ex(&mut s, "undo").unwrap_err().to_string(),
            "nothing to undo"
        );
        assert!(!s.datasets()[1].is_modified());
        let renamed = ex(&mut s, "tree rename --tip A --to Q").unwrap().message;
        assert!(
            renamed.ends_with("; 2 of 3 tips have a sequence"),
            "{renamed}"
        );
        assert_eq!(
            ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap().message,
            "linked t.nwk to a.fa: 2 of 3 tips have a sequence; tips without one: Q; 2 sequences have no tip: A, Z"
        );
        assert_eq!(
            ex(&mut s, "tree unlink").unwrap().message,
            "unlinked t.nwk from a.fa"
        );
        assert_eq!(
            ex(&mut s, "tree unlink").unwrap_err().to_string(),
            "t.nwk is not linked"
        );
        assert_eq!(s.link_of(s.datasets()[1].id), None);
        ex(&mut s, "use 1").unwrap();
        assert_eq!(
            ex_in(&mut s, &dir, "tree link {}/a.fa")
                .unwrap_err()
                .to_string(),
            "a.fa is not a tree; switch with use N"
        );
    }

    #[test]
    fn a_failed_log_write_removes_both_extracted_files_and_undoes_a_link() {
        let (_keep, dir) = trees();
        let lines = [
            format!("open {}", dir.join("a.fa").display()),
            format!("open {}", dir.join("t.nwk").display()),
            format!("tree link {}", dir.join("a.fa").display()),
        ];
        let room = |count: usize| {
            let (log, buffer) = limited_log(100_000);
            let mut s = Session::with_clock(Some(log), fixed);
            for line in &lines[..count] {
                ex(&mut s, line).unwrap();
            }
            limited_log(buffer.lock().unwrap().len() + 10).0
        };
        let mut s = Session::with_clock(Some(room(2)), fixed);
        for line in &lines[..2] {
            ex(&mut s, line).unwrap();
        }
        assert!(matches!(ex(&mut s, &lines[2]), Err(Error::Log { .. })));
        assert_eq!(s.link_of(s.datasets()[1].id), None);

        let mut s = Session::with_clock(Some(room(3)), fixed);
        for line in &lines {
            ex(&mut s, line).unwrap();
        }
        let err = ex_in(&mut s, &dir, "tree extract --out {}/s.nwk --seqs {}/s.fa").unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert!(!dir.join("s.nwk").exists() && !dir.join("s.fa").exists());
    }

    #[test]
    fn kinds_do_not_mix() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "t.nwk"]);
        assert_eq!(
            ex(&mut s, "msa delete-cols --cols 1")
                .unwrap_err()
                .to_string(),
            "t.nwk is not an alignment; switch with use N"
        );
        assert_eq!(
            ex(&mut s, "undo").unwrap_err().to_string(),
            "nothing to undo"
        );
        assert_eq!(
            ex(&mut s, "redo").unwrap_err().to_string(),
            "nothing to redo"
        );
        ex(&mut s, "use 1").unwrap();
        assert_eq!(
            ex(&mut s, "tree ladderize").unwrap_err().to_string(),
            "a.fa is not a tree; switch with use N"
        );
    }

    #[test]
    fn the_log_records_commands_notes_and_outputs() {
        let (_keep, dir) = trio_dir();
        let log = dir.join("s.log");
        let out = dir.join("out.fa");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        s.execute(open(&dir.join("a.fa"))).unwrap();
        ex(&mut s, "msa delete-rows --rows 1").unwrap();
        ex(&mut s, "msa rename --rows 1 --to two").unwrap();
        ex(&mut s, &format!("msa extract --out {}", out.display())).unwrap();

        let text = fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let commands: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| !l.starts_with('#'))
            .collect();
        assert_eq!(commands, s.history());
        let after = |command: &str| lines[lines.iter().position(|l| *l == command).unwrap() + 1];
        assert_eq!(after("msa delete-rows --rows 1"), "# removed: s1");
        assert_eq!(after("msa rename --rows 1 --to two"), "# was: s2");
        assert_eq!(
            after(commands[3]),
            format!(
                "# output {} size=41",
                fs::canonicalize(&out).unwrap().display()
            )
        );
    }

    #[test]
    fn refused_commands_are_not_logged() {
        let (_keep, dir) = trio_dir();
        let log = dir.join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        s.execute(open(&dir.join("a.fa"))).unwrap();
        assert!(ex(&mut s, "msa delete-cols --cols 99").is_err());
        assert_eq!(s.history().len(), 1);
        let text = fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().filter(|l| !l.starts_with('#')).count(), 1);
    }

    /// A log with room for the header and one `open` of `file`, and no more.
    fn log_full_after_open(file: &Path) -> SessionLog {
        let (log, buffer) = limited_log(100_000);
        let mut s = Session::with_clock(Some(log), fixed);
        s.execute(open(file)).unwrap();
        let used = buffer.lock().unwrap().len();
        limited_log(used + 10).0
    }

    #[test]
    fn a_failed_log_write_undoes_an_edit() {
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        let mut s = Session::with_clock(Some(log_full_after_open(&file)), fixed);
        s.execute(open(&file)).unwrap();
        let err = ex(&mut s, "msa delete-cols --cols 1-3").unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert_eq!(cols(&s, 0), 10);
        assert_eq!(s.history().len(), 1);
        let err = ex(&mut s, "msa delete-cols --cols 1").unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert_eq!(cols(&s, 0), 10);
    }

    #[test]
    fn a_failed_log_write_removes_the_extracted_file() {
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        let out = dir.join("out.fa");
        let mut s = Session::with_clock(Some(log_full_after_open(&file)), fixed);
        s.execute(open(&file)).unwrap();
        let err = ex(&mut s, &format!("msa extract --out {}", out.display())).unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert!(!out.exists());
        assert_eq!(fs::read_to_string(&file).unwrap(), TRIO);
    }

    #[test]
    fn a_failed_log_write_undoes_use_and_open() {
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        let mut s = Session::with_clock(Some(log_full_after_open(&file)), fixed);
        s.execute(open(&file)).unwrap();
        assert!(s.execute(open(&dir.join("b.fa"))).is_err());
        assert_eq!((s.datasets().len(), s.current()), (1, Some(0)));
    }

    #[test]
    fn extract_onto_an_open_input_is_refused_and_not_logged() {
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        let mut s = opened(&dir, &["a.fa"]);
        ex(&mut s, "msa delete-cols --cols 1").unwrap();
        let err = ex(&mut s, &format!("msa extract --out {}", file.display())).unwrap_err();
        assert!(matches!(err, Error::Exists(_)), "{err}");
        assert_eq!(fs::read_to_string(&file).unwrap(), TRIO);
        assert_eq!(s.history().len(), 2);
    }

    #[test]
    fn a_command_that_does_not_read_back_is_refused() {
        use crate::{MsaCmd, RowSel};
        let (_keep, dir) = trio_dir();
        let mut session = opened(&dir, &["a.fa"]);
        // `--to` and `--regex` cannot be combined, so this text does not parse.
        let odd = Command::Msa(MsaCmd::Rename {
            rows: RowSel::default(),
            to: Some("x".into()),
            regex: Some("a".into()),
            replace: Some("b".into()),
            tree: false,
        });
        let error = session.execute(odd).unwrap_err().to_string();
        assert!(error.starts_with("internal error: "), "{error}");
        assert_eq!(session.history().len(), 1);
        assert_eq!(session.datasets()[0].msa().unwrap().name(0), "s1");
    }

    fn modified(session: &Session) -> bool {
        session.datasets()[0].msa().unwrap().is_modified()
    }

    #[test]
    fn an_alignment_is_modified_until_it_is_written() {
        let (_keep, dir) = trio_dir();
        let mut session = opened(&dir, &["a.fa"]);
        assert!(!modified(&session));
        ex(&mut session, "msa rename --rows 1 --to x").unwrap();
        assert!(modified(&session));
        ex(&mut session, "undo").unwrap();
        assert!(!modified(&session), "back at the opened state");
        ex(&mut session, "redo").unwrap();
        let out = dir.join("o.fa");
        ex(
            &mut session,
            &format!("msa extract --rows 1 --out {}", out.display()),
        )
        .unwrap();
        assert!(!modified(&session), "any extract counts as written");
        ex(&mut session, "undo").unwrap();
        assert!(
            modified(&session),
            "the state now differs from the one written"
        );
    }

    fn ids(session: &Session) -> Vec<u64> {
        session.datasets().iter().map(|d| d.id).collect()
    }

    #[test]
    fn close_removes_the_current_file() {
        let (_keep, dir) = trio_dir();
        fs::write(dir.join("c.fa"), TRIO).unwrap();
        let mut session = opened(&dir, &["a.fa", "b.fa", "c.fa"]);
        assert_eq!(ids(&session), [1, 2, 3]);
        ex(&mut session, "use 2").unwrap();
        assert_eq!(
            ex(&mut session, "close").unwrap().message,
            "closed b.fa; using 2: c.fa"
        );
        assert_eq!(ids(&session), [1, 3]);
        assert_eq!(session.current(), Some(1));
        assert_eq!(
            ex(&mut session, "close").unwrap().message,
            "closed c.fa; using 1: a.fa"
        );
        assert_eq!(ex(&mut session, "close").unwrap().message, "closed a.fa");
        assert_eq!((session.datasets().len(), session.current()), (0, None));
        assert_eq!(
            ex(&mut session, "close").unwrap_err().to_string(),
            "no file is open"
        );
        assert_eq!(session.history().last().unwrap(), "close");
        // Ids are never reused.
        session.execute(open(&dir.join("a.fa"))).unwrap();
        assert_eq!(ids(&session), [4]);
    }

    #[test]
    fn a_close_that_cannot_be_logged_is_put_back() {
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        let mut session = Session::with_clock(Some(log_full_after_open(&file)), fixed);
        session.execute(open(&file)).unwrap();
        assert!(matches!(ex(&mut session, "close"), Err(Error::Log { .. })));
        assert_eq!((session.datasets().len(), session.current()), (1, Some(0)));
        assert_eq!(ids(&session), [1]);
    }

    fn running(started: Started) -> Job {
        match started {
            Started::Running(job) => job,
            Started::Done(outcome) => panic!("finished at once: {}", outcome.message),
        }
    }

    fn start(session: &mut Session, line: &str) -> crate::Result<Started> {
        session.start(Command::parse_line(line).unwrap())
    }

    #[test]
    fn jobs_can_cross_threads() {
        fn sendable<T: Send + 'static>() {}
        sendable::<Job>();
        sendable::<Finished>();
    }

    #[test]
    fn a_slow_command_changes_nothing_until_it_is_finished() {
        let (_keep, dir) = trio_dir();
        let mut session = Session::new(None);
        let job = running(session.start(open(&dir.join("a.fa"))).unwrap());
        assert_eq!(job.label(), "opening a.fa");
        assert!(session.is_busy() && session.datasets().is_empty() && session.history().is_empty());
        let progress = job.progress();
        let outcome = session.finish(job.run()).unwrap();
        assert!(
            outcome.message.ends_with("(msa, 3 sequences x 10 columns)"),
            "{}",
            outcome.message
        );
        assert_eq!(progress.fraction(), Some(1.0));
        assert!(!session.is_busy());
        assert_eq!((session.datasets().len(), session.history().len()), (1, 1));
    }

    #[test]
    fn quick_commands_are_done_at_once() {
        let (_keep, dir) = trio_dir();
        let mut session = opened(&dir, &["a.fa"]);
        for line in ["msa delete-cols --cols 1", "undo", "use 1", "close"] {
            let started = start(&mut session, line).unwrap();
            assert!(matches!(started, Started::Done(_)), "{line}");
        }
        let line = format!("open {}", dir.join("t.nwk").display());
        let started = start(&mut session, &line).unwrap();
        assert!(
            matches!(started, Started::Running(_)),
            "a tree is read in the background"
        );
    }

    #[test]
    fn nothing_else_starts_while_a_job_is_out() {
        let (_keep, dir) = trio_dir();
        let mut session = opened(&dir, &["a.fa"]);
        let out = dir.join("o.fa");
        let job = running(
            start(
                &mut session,
                &format!("msa extract --out {}", out.display()),
            )
            .unwrap(),
        );
        assert_eq!(job.label(), "writing o.fa");
        for line in [
            "undo",
            "msa delete-cols --cols 1",
            &format!("open {}", dir.join("a.fa").display()),
        ] {
            let refused = start(&mut session, line).err().unwrap();
            assert_eq!(refused.to_string(), "still busy: writing o.fa");
        }
        assert_eq!(
            session.finish(job.run()).unwrap().message,
            format!("wrote {} (3 sequences x 10 columns)", out.display())
        );
        assert_eq!(fs::read_to_string(&out).unwrap(), TRIO);
        assert_eq!(
            session.start(Command::Undo).err().unwrap().to_string(),
            "nothing to undo",
            "refused for its own reason: the session is free again"
        );
        assert!(!session.is_busy());
    }

    #[test]
    fn a_command_refused_at_start_leaves_the_session_free() {
        let (_keep, dir) = trio_dir();
        let mut session = opened(&dir, &["a.fa"]);
        let refused = start(&mut session, "msa extract --cols 99 --out x.fa")
            .err()
            .unwrap();
        assert_eq!(
            refused.to_string(),
            "column 99 is past the last column (10)"
        );
        assert!(!session.is_busy());
    }

    #[test]
    fn a_cancelled_open_leaves_nothing() {
        let (_keep, dir) = trio_dir();
        let log = dir.join("s.log");
        let mut session = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        let job = running(session.start(open(&dir.join("a.fa"))).unwrap());
        job.progress().cancel();
        assert!(matches!(session.finish(job.run()), Err(Error::Cancelled)));
        assert!(session.datasets().is_empty() && session.history().is_empty());
        assert!(!session.is_busy());
        drop(session);
        assert!(!log.exists(), "a log with no commands is removed");
    }

    #[test]
    fn a_cancelled_open_of_a_small_tree_leaves_nothing() {
        let (_keep, dir) = trees();
        let mut session = Session::new(None);
        let job = running(session.start(open(&dir.join("t.nwk"))).unwrap());
        job.progress().cancel();
        assert!(matches!(session.finish(job.run()), Err(Error::Cancelled)));
        assert!(session.datasets().is_empty() && session.history().is_empty());
    }

    #[test]
    fn a_cancelled_extract_leaves_no_file_and_the_alignment_unwritten() {
        let (_keep, dir) = trio_dir();
        let mut session = opened(&dir, &["a.fa"]);
        ex(&mut session, "msa delete-cols --cols 1").unwrap();
        let files = || fs::read_dir(&dir).unwrap().count();
        let before = files();
        let line = format!("msa extract --out {}", dir.join("o.fa").display());
        let job = running(start(&mut session, &line).unwrap());
        job.progress().cancel();
        assert_eq!(
            session.finish(job.run()).unwrap_err().to_string(),
            "cancelled"
        );
        assert_eq!(files(), before, "no output and no partial file");
        assert!(session.datasets()[0].msa().unwrap().is_modified());
        assert_eq!(session.history().len(), 2);
    }

    #[test]
    fn rescue_writes_the_alignment_once_the_log_has_failed() {
        const WORKING: &str = "the session log has not failed; use msa extract --out PATH";
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        let out = dir.join("saved.fa");
        let mut session = Session::with_clock(Some(log_full_after_open(&file)), fixed);
        session.execute(open(&file)).unwrap();
        assert_eq!(session.rescue(0, &out).unwrap_err().to_string(), WORKING);
        assert!(matches!(
            ex(&mut session, "msa delete-cols --cols 1"),
            Err(Error::Log { .. })
        ));
        assert!(session.log_failed());
        assert_eq!(
            session.rescue(0, &out).unwrap().message,
            format!(
                "wrote {} from a.fa (3 sequences x 10 columns); not logged, because the session log could not be written",
                out.display()
            )
        );
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            TRIO,
            "the refused edit was undone"
        );
        assert!(matches!(session.rescue(0, &out), Err(Error::Exists(_))));

        let no_log = Session::new(None);
        assert_eq!(no_log.rescue(0, &out).unwrap_err().to_string(), WORKING);
    }

    #[test]
    fn rescue_writes_the_file_it_is_given_not_the_current_one() {
        let (_keep, dir) = trio_dir();
        let (a, b) = (dir.join("a.fa"), dir.join("b.fa"));
        let script = |s: &mut Session| {
            s.execute(open(&a)).unwrap();
            ex(s, "msa fill --rows 1 --cols 1 --with N").unwrap();
            s.execute(open(&b)).unwrap();
        };
        let (log, buffer) = limited_log(100_000);
        script(&mut Session::with_clock(Some(log), fixed));
        let used = buffer.lock().unwrap().len();
        let mut s = Session::with_clock(Some(limited_log(used + 10).0), fixed);
        script(&mut s);
        assert!(matches!(ex(&mut s, "use 1"), Err(Error::Log { .. })));
        assert_eq!(s.current(), Some(1), "the session is stuck on b.fa");

        let out = dir.join("saved.fa");
        let message = s.rescue(0, &out).unwrap().message;
        assert!(
            message.starts_with(&format!("wrote {} from a.fa (", out.display())),
            "{message}"
        );
        assert!(
            fs::read_to_string(&out)
                .unwrap()
                .starts_with(">s1\nNCGT-ACGTA\n"),
            "a.fa's edit is in the file"
        );
        assert_eq!(
            s.rescue(5, &dir.join("x.fa")).unwrap_err().to_string(),
            "no file is open"
        );
    }

    #[test]
    fn check_input_reports_unknown_kinds_before_missing_files() {
        let (_keep, dir) = trio_dir();
        let file = dir.join("a.fa");
        assert_eq!(check_input(&file, None).unwrap(), Kind::Msa);
        assert_eq!(check_input(&file, Some(Kind::Tree)).unwrap(), Kind::Tree);
        assert!(matches!(
            check_input(&dir.join("x.txt"), None),
            Err(Error::UnknownKind(_))
        ));
        assert!(matches!(
            check_input(&dir.join("x.fa"), None),
            Err(Error::Io { .. })
        ));
    }

    #[test]
    fn prune_with_seqs_deletes_the_clades_rows_too() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "l.nwk"]);
        assert_eq!(
            ex(&mut s, "tree prune --tip A --seqs")
                .unwrap_err()
                .to_string(),
            "--seqs needs the tree linked to an alignment: tree link FILE"
        );
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        let out = ex(&mut s, "tree prune --mrca C --mrca D --seqs").unwrap();
        assert_eq!(
            out.message,
            "pruned 2 tips; now 2 tips; deleted 1 row from a.fa (1 tip had no sequence); \
             2 of 2 tips have a sequence"
        );
        let a = s.datasets()[0].msa().unwrap();
        assert_eq!(
            (0..a.rows()).map(|r| a.name(r)).collect::<Vec<_>>(),
            ["A", "B", "Z"]
        );
        assert_eq!(
            s.history().last().unwrap(),
            "tree prune --mrca C --mrca D --seqs"
        );
    }

    #[test]
    fn prune_with_seqs_deletes_every_copy_and_is_refused_whole() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["two.fa", "l.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/two.fa").unwrap();
        // A and B are every row but C: fine. Then C and D would empty two.fa.
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        assert_eq!(s.datasets()[0].msa().unwrap().rows(), 1);
        let err = ex(&mut s, "tree prune --mrca C --mrca D --seqs").unwrap_err();
        assert_eq!(err.to_string(), "two.fa: that would delete every sequence");
        assert_eq!(
            s.datasets()[1].tree().unwrap().tips(),
            2,
            "the tree is untouched too"
        );
    }

    #[test]
    fn a_clade_with_no_sequences_prunes_the_tree_alone() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "l.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        let out = ex(&mut s, "tree prune --tip D --seqs").unwrap();
        assert!(
            out.message.contains("; no row of a.fa carried those names"),
            "{}",
            out.message
        );
        assert_eq!(
            s.datasets()[1].tree().unwrap().undo_pair(),
            None,
            "decision 1"
        );
    }

    /// a.fa and l.nwk open (tree current) and linked.
    fn linked(dir: &Path) -> Session {
        let mut s = opened(dir, &["a.fa", "l.nwk"]);
        ex_in(&mut s, dir, "tree link {}/a.fa").unwrap();
        s
    }

    fn sizes(s: &Session) -> (usize, usize) {
        (
            s.datasets()[0].msa().unwrap().rows(),
            s.datasets()[1].tree().unwrap().tips(),
        )
    }

    #[test]
    fn one_undo_reverses_both_files_from_either_tab() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        ex(&mut s, "use 1").unwrap();
        let out = ex(&mut s, "undo").unwrap();
        assert_eq!(
            out.message,
            "undid prune; now 4 sequences x 4 columns; and in l.nwk: undid prune; now 4 tips"
        );
        ex(&mut s, "use 2").unwrap();
        ex(&mut s, "redo").unwrap();
        assert_eq!(sizes(&s), (2, 2));
    }

    #[test]
    fn a_later_edit_in_the_other_file_comes_first() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "msa delete-cols --cols 1").unwrap();
        ex(&mut s, "use 2").unwrap();
        assert_eq!(
            ex(&mut s, "undo").unwrap_err().to_string(),
            "undo the later edit in a.fa first"
        );
        assert_eq!(sizes(&s), (2, 2), "nothing changed");
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "undo").unwrap(); // the column
        ex(&mut s, "undo").unwrap(); // the pair, from the alignment's tab
        assert_eq!(sizes(&s), (4, 4));
    }

    #[test]
    fn redo_is_refused_once_the_other_file_has_moved_on() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        ex(&mut s, "undo").unwrap();
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "msa delete-cols --cols 1").unwrap();
        ex(&mut s, "use 2").unwrap();
        assert_eq!(
            ex(&mut s, "redo").unwrap_err().to_string(),
            "a.fa has been edited since, so this cannot be redone in both"
        );
        assert_eq!(sizes(&s), (4, 4));
    }

    #[test]
    fn a_step_the_other_file_has_dropped_cannot_be_undone() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        ex(&mut s, "use 1").unwrap();
        for i in 0..20 {
            let with = if i % 2 == 0 { "N" } else { "A" };
            ex(&mut s, &format!("msa fill --rows 1 --cols 1 --with {with}")).unwrap();
        }
        ex(&mut s, "use 2").unwrap();
        assert_eq!(
            ex(&mut s, "undo").unwrap_err().to_string(),
            "a.fa no longer keeps that step, so it cannot be undone in both"
        );
        assert_eq!(sizes(&s), (2, 2));
    }

    #[test]
    fn closing_the_other_file_leaves_this_half_to_undo_alone() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        ex(&mut s, "tree unlink").unwrap();
        ex(&mut s, "undo").unwrap();
        assert_eq!(sizes(&s), (4, 4), "unlinking does not split the step");
        ex(&mut s, "redo").unwrap();
        assert_eq!(sizes(&s), (2, 2));
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "close").unwrap();
        let out = ex(&mut s, "undo").unwrap();
        assert!(
            out.message
                .ends_with("; the other file is closed, so only this one changed"),
            "{}",
            out.message
        );
        assert_eq!(s.datasets()[0].tree().unwrap().tips(), 4);
    }

    #[test]
    fn a_failed_log_write_undoes_both_files() {
        let (_keep, dir) = trees();
        // A log with room for the lines `steps` make and a little more.
        let full_after = |steps: &[&str]| {
            let (log, buffer) = limited_log(100_000);
            let mut s = Session::with_clock(Some(log), fixed);
            s.execute(open(&dir.join("a.fa"))).unwrap();
            s.execute(open(&dir.join("l.nwk"))).unwrap();
            for line in steps {
                ex_in(&mut s, &dir, line).unwrap();
            }
            let used = buffer.lock().unwrap().len();
            let mut s = Session::with_clock(Some(limited_log(used + 10).0), fixed);
            s.execute(open(&dir.join("a.fa"))).unwrap();
            s.execute(open(&dir.join("l.nwk"))).unwrap();
            for line in steps {
                ex_in(&mut s, &dir, line).unwrap();
            }
            s
        };
        let mut s = full_after(&["tree link {}/a.fa"]);
        let err = ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert_eq!(sizes(&s), (4, 4), "both files as they were");

        let mut s = full_after(&["tree link {}/a.fa", "tree prune --mrca A --mrca B --seqs"]);
        let err = ex(&mut s, "undo").unwrap_err();
        assert!(matches!(err, Error::Log { .. }), "{err}");
        assert_eq!(sizes(&s), (2, 2), "the undo did not stand in either file");
    }

    #[test]
    fn a_tip_rename_renames_its_sequences_and_back() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["two.fa", "l.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/two.fa").unwrap();
        let out = ex(&mut s, "tree rename --tip A --to Q --seqs").unwrap();
        assert!(
            out.message.contains("; renamed 2 rows in two.fa"),
            "{}",
            out.message
        );
        let a = s.datasets()[0].msa().unwrap();
        assert_eq!((a.name(0), a.name(3)), ("Q", "Q"), "every copy");
        ex(&mut s, "use 1").unwrap();
        let out = ex(&mut s, "msa rename --rows 2 --to R --tree").unwrap();
        assert!(
            out.message.ends_with("; renamed 1 tip in l.nwk"),
            "{}",
            out.message
        );
        let tree = s.datasets()[1].tree().unwrap().tree();
        assert!(tree.tip_named("R").is_ok());
        let out = ex(&mut s, "undo").unwrap();
        assert!(out.message.contains("; and in l.nwk:"), "{}", out.message);
        assert!(
            s.datasets()[1]
                .tree()
                .unwrap()
                .tree()
                .tip_named("B")
                .is_ok()
        );
    }

    #[test]
    fn a_regex_rename_reaches_every_row_and_is_checked_first() {
        let (_keep, dir) = trees();
        fs::write(dir.join("z.fa"), ">A_1\nAC\n>B_2\nGG\n>x\nTT\n").unwrap();
        fs::write(dir.join("u.nwk"), "(A_1,(B_2,C_3));").unwrap();
        let mut s = opened(&dir, &["z.fa", "u.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/z.fa").unwrap();
        let out = ex(&mut s, "tree rename --regex _ --replace ' ' --seqs").unwrap();
        assert!(
            out.message.contains("; renamed 2 rows in z.fa"),
            "{}",
            out.message
        );
        let names = |s: &Session| {
            let a = s.datasets()[0].msa().unwrap();
            (0..a.rows())
                .map(|r| a.name(r).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&s), ["A 1", "B 2", "x"]);
        assert!(
            s.datasets()[1]
                .tree()
                .unwrap()
                .tree()
                .tip_named("C 3")
                .is_ok()
        );

        // Only the alignment has x: its check refuses before the tree changes.
        assert_eq!(
            ex(&mut s, "tree rename --regex '^x$' --replace '' --seqs")
                .unwrap_err()
                .to_string(),
            "a name cannot be empty (row 3)"
        );
        assert_eq!(names(&s), ["A 1", "B 2", "x"]);

        // Only the tree has C: an edit to the tree alone.
        let out = ex(&mut s, "tree rename --regex '^C' --replace D --seqs").unwrap();
        assert!(
            out.message.contains("; no row of z.fa was renamed"),
            "{}",
            out.message
        );
        assert_eq!(s.datasets()[1].tree().unwrap().undo_pair(), None);
    }

    #[test]
    fn a_rename_that_would_share_a_new_tip_name_is_refused_in_both() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa", "dup.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        ex(&mut s, "use 1").unwrap();
        // Tips that already share a name may go on sharing a new one (2a).
        let out = ex(&mut s, "msa rename --name A --to N --tree").unwrap();
        assert!(
            out.message.ends_with("; renamed 2 tips in dup.nwk"),
            "{}",
            out.message
        );
        // But not a name another tip has: refused before either file changes.
        let err = ex(&mut s, "msa rename --name N --to B --tree").unwrap_err();
        assert!(err.to_string().contains("'B'"), "{err}");
        assert_eq!(s.datasets()[0].msa().unwrap().name(0), "N");
        assert!(
            s.datasets()[1]
                .tree()
                .unwrap()
                .tree()
                .tip_named("N")
                .is_err(),
            "N is shared"
        );
        assert_eq!(
            s.datasets()[1].tree().unwrap().undo_pair().map(|p| p.id),
            Some(1)
        );
    }

    #[test]
    fn tree_needs_one_linked_tree() {
        let (_keep, dir) = trees();
        let mut s = opened(&dir, &["a.fa"]);
        assert_eq!(
            ex(&mut s, "msa rename --name A --to Q --tree")
                .unwrap_err()
                .to_string(),
            "--tree needs this alignment linked to a tree: tree link FILE from the tree's tab"
        );
        ex_in(&mut s, &dir, "open {}/t.nwk").unwrap();
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        ex_in(&mut s, &dir, "open {}/l.nwk").unwrap();
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        ex(&mut s, "use 1").unwrap();
        assert_eq!(
            ex(&mut s, "msa rename --name A --to Q --tree")
                .unwrap_err()
                .to_string(),
            "a.fa is linked to 2 trees; --tree needs exactly one"
        );
    }

    #[test]
    fn a_linked_rename_refuses_to_give_two_rows_one_name() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        const CLASH: &str = "a.fa: that would give 'Z' to more than one sequence";
        let err = ex(&mut s, "tree rename --tip A --to Z --seqs").unwrap_err();
        assert_eq!(err.to_string(), CLASH);
        assert!(
            s.datasets()[1]
                .tree()
                .unwrap()
                .tree()
                .tip_named("A")
                .is_ok()
        );
        ex(&mut s, "use 1").unwrap();
        let err = ex(&mut s, "msa rename --name A --to Z --tree").unwrap_err();
        assert_eq!(err.to_string(), CLASH);
        assert_eq!(s.datasets()[0].msa().unwrap().name(0), "A");

        // Two names merged into one by a regex.
        fs::write(dir.join("m.fa"), ">A_1\nAC\n>A-1\nGG\n").unwrap();
        fs::write(dir.join("m.nwk"), "(A_1,B,C);").unwrap();
        let mut s = opened(&dir, &["m.fa", "m.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/m.fa").unwrap();
        assert_eq!(
            ex(&mut s, "tree rename --regex '[_-]' --replace ' ' --seqs")
                .unwrap_err()
                .to_string(),
            "m.fa: that would give 'A 1' to more than one sequence"
        );
        assert!(
            s.datasets()[1]
                .tree()
                .unwrap()
                .tree()
                .tip_named("A_1")
                .is_ok()
        );
    }

    #[test]
    fn redo_advice_points_at_the_step_in_the_way() {
        let (_keep, dir) = trees();
        let mut s = linked(&dir);
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "msa delete-cols --cols 1").unwrap();
        ex(&mut s, "use 2").unwrap();
        ex(&mut s, "tree prune --mrca A --mrca B --seqs").unwrap();
        ex(&mut s, "undo").unwrap();
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "undo").unwrap();
        ex(&mut s, "use 2").unwrap();
        assert_eq!(
            ex(&mut s, "redo").unwrap_err().to_string(),
            "redo in a.fa first"
        );
        ex(&mut s, "use 1").unwrap();
        ex(&mut s, "redo").unwrap();
        ex(&mut s, "use 2").unwrap();
        ex(&mut s, "redo").unwrap();
        assert_eq!(sizes(&s), (2, 2));
    }

    #[test]
    fn a_linked_rename_reaches_every_copy_on_both_sides() {
        let (_keep, dir) = trees();
        // One of two tips named A, renamed from the tree: both tips follow
        // the row, so neither tip loses its sequence.
        let mut s = opened(&dir, &["a.fa", "dup.nwk"]);
        ex_in(&mut s, &dir, "tree link {}/a.fa").unwrap();
        ex(&mut s, "tree rename --node 3 --to Q --seqs").unwrap();
        let tips = |s: &Session| {
            let tree = s.datasets()[1].tree().unwrap().tree();
            let mut names: Vec<String> = tree
                .tips()
                .into_iter()
                .map(|n| tree.label(n).unwrap_or_default().to_string())
                .collect();
            names.sort();
            names
        };
        assert_eq!(tips(&s), ["B", "Q", "Q"]);
        assert_eq!(s.datasets()[0].msa().unwrap().name(0), "Q");
        ex(&mut s, "undo").unwrap();
        assert_eq!(tips(&s), ["A", "A", "B"]);

        // One of two rows named A, renamed from the alignment: both rows.
        fs::write(dir.join("aab.fa"), ">A\nAC\n>A\nGG\n>B\nTT\n").unwrap();
        fs::write(dir.join("ab.nwk"), "(A,B);").unwrap();
        let names = |s: &Session| {
            let a = s.datasets()[0].msa().unwrap();
            (0..a.rows())
                .map(|r| a.name(r).to_string())
                .collect::<Vec<_>>()
        };
        for line in [
            "msa rename --rows 1 --to Q --tree",
            "msa rename --name A --to Q --tree",
            "msa rename --rows 2 --regex A --replace Q --tree",
        ] {
            let mut s = opened(&dir, &["aab.fa", "ab.nwk"]);
            ex_in(&mut s, &dir, "tree link {}/aab.fa").unwrap();
            ex(&mut s, "use 1").unwrap();
            ex(&mut s, line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(names(&s), ["Q", "Q", "B"], "{line}");
            assert!(
                s.datasets()[1]
                    .tree()
                    .unwrap()
                    .tree()
                    .tip_named("Q")
                    .is_ok(),
                "{line}"
            );
            ex(&mut s, "undo").unwrap();
            assert_eq!(names(&s), ["A", "A", "B"], "{line}");
        }
    }
}
