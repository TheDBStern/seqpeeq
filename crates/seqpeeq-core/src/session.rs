//! A session: the open datasets, and the one place commands are executed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::{Timestamp, Zoned};

use crate::log::stamp;
use crate::msa::layer::Layer;
use crate::msa::{Alignment, Applied, Extraction, write};
use crate::progress::Progress;
use crate::{Command, Error, Kind, MsaCmd, Result, SessionLog};

/// What an open file holds.
#[derive(Debug)]
pub enum Content {
    // Boxed so that a list of datasets stays small whatever they hold.
    Msa(Box<Alignment>),
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
            Content::Unloaded => None,
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
            Work::Open(input) => {
                Alignment::load_with(&input.path, &self.progress).map(|alignment| Product::Opened {
                    input,
                    alignment: Box::new(alignment),
                })
            }
            Work::Extract {
                dataset,
                extraction,
            } => extraction
                .run(&self.progress)
                .map(|applied| Product::Wrote { dataset, applied }),
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

enum Product {
    Opened {
        input: Input,
        alignment: Box<Alignment>,
    },
    Wrote {
        dataset: u64,
        applied: Applied,
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
}

/// Enough of the state before a command to put it back if the command
/// cannot be logged.
struct Checkpoint {
    datasets: usize,
    current: Option<usize>,
    /// The edits of the current alignment, when the command may change them.
    layer: Option<Layer>,
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
        }
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
                let input = Input::check(&path, kind, &now)?;
                if input.kind == Kind::Msa {
                    let label = format!("opening {}", file_name(&input.path));
                    return Ok(self.hand_out(label, line, Work::Open(input)));
                }
                // No viewer yet, so there is nothing slow to read.
                let message = input.opened(&format!("{} bytes", input.size));
                return self
                    .record_open(&line, input, Content::Unloaded, message, &now)
                    .map(Started::Done);
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
            Product::Opened { input, alignment } => {
                let message = input.opened(&alignment.shape());
                let content = Content::Msa(alignment);
                self.record_open(&line, input, content, message, &now)
            }
            Product::Wrote { dataset, applied } => {
                if let Some(log) = &mut self.log
                    && let Err(error) = log.record(&line, applied.note.as_deref(), &now)
                {
                    // A file that is not in the log must not stand.
                    if let Some(path) = &applied.created {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(error);
                }
                let written = self.datasets.iter_mut().find(|d| d.id == dataset);
                if let Some(Content::Msa(alignment)) = written.map(|d| &mut d.content) {
                    alignment.mark_written();
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
        let edits = matches!(cmd, Command::Msa(_) | Command::Undo | Command::Redo);
        let alignment = self
            .current
            .and_then(|index| self.datasets[index].msa())
            .filter(|_| edits);
        Checkpoint {
            datasets: self.datasets.len(),
            current: self.current,
            layer: alignment.map(Alignment::snapshot),
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
        if let Some(layer) = checkpoint.layer
            && let Ok(alignment) = self.current_alignment()
        {
            alignment.restore(layer);
        }
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
            Content::Unloaded => Err(Error::Invalid(format!(
                "{name} is not an alignment; switch with use N"
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
        let applied = Applied {
            message,
            note: None,
            created: None,
        };
        Ok((applied, at, dataset))
    }

    /// Apply a quick command to the session state.
    fn apply(&mut self, cmd: Command) -> Result<Applied> {
        match cmd {
            Command::Open { .. } | Command::Close => {
                unreachable!("start handles opening and closing itself")
            }
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
                    created: None,
                })
            }
            Command::Undo => self.current_alignment()?.undo(),
            Command::Redo => self.current_alignment()?.redo(),
            Command::Msa(cmd) => self.current_alignment()?.apply(&cmd),
        }
    }
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
        assert!(out.message.ends_with("(tree, 6 bytes)"), "{}", out.message);
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
        for line in ["msa delete-cols --cols 1", "undo"] {
            assert_eq!(
                ex(&mut s, line).unwrap_err().to_string(),
                "t.nwk is not an alignment; switch with use N"
            );
        }
        assert_eq!(cols(&s, 0), 10);
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
        for line in [
            "msa delete-cols --cols 1",
            "undo",
            "use 1",
            &format!("open {}", dir.join("t.nwk").display()),
            "close",
        ] {
            let started = start(&mut session, line).unwrap();
            assert!(matches!(started, Started::Done(_)), "{line}");
        }
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
}
