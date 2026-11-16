//! A session: the open datasets, and the one place commands are executed.

use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};

use crate::log::stamp;
use crate::msa::layer::Layer;
use crate::msa::{Alignment, Applied};
use crate::{Command, Error, Kind, Result, SessionLog};

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
        match self.path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.path.display().to_string(),
        }
    }
}

/// What a successfully executed command reports back to the interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub message: String,
}

pub struct Session {
    datasets: Vec<Dataset>,
    /// The dataset that data commands apply to.
    current: Option<usize>,
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
            history: Vec::new(),
            log,
            clock,
        }
    }

    /// Run a command and, if it succeeds, record it.
    ///
    /// The log is opened before the command is applied, so a session that
    /// cannot be logged refuses the command instead of changing state silently.
    pub fn execute(&mut self, cmd: Command) -> Result<Outcome> {
        let line = cmd.to_line()?;
        let now = (self.clock)();
        if let Some(log) = &mut self.log {
            log.ensure_open(&now)?;
        }
        let checkpoint = self.checkpoint(&cmd);
        let applied = self.apply(cmd, &now)?;
        if let Some(log) = &mut self.log
            && let Err(error) = log.record(&line, applied.note.as_deref(), &now)
        {
            // A change that is not in the log must not stand: undo it. The
            // log is now marked incomplete, so later commands are refused.
            self.roll_back(checkpoint, applied.created.as_deref());
            return Err(error);
        }
        self.history.push(line);
        Ok(Outcome {
            message: applied.message,
        })
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

    /// Put the state back to `checkpoint` and remove a file the command
    /// created. Undo history is not repaired: the log has failed, so no
    /// further command, `undo` included, will be accepted.
    fn roll_back(&mut self, checkpoint: Checkpoint, created: Option<&Path>) {
        self.datasets.truncate(checkpoint.datasets);
        self.current = checkpoint.current;
        if let Some(layer) = checkpoint.layer
            && let Ok(alignment) = self.current_alignment()
        {
            alignment.restore(layer);
        }
        if let Some(path) = created {
            let _ = std::fs::remove_file(path);
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

    /// Apply a command to the session state.
    fn apply(&mut self, cmd: Command, now: &Zoned) -> Result<Applied> {
        match cmd {
            Command::Open { path, kind } => {
                let kind = kind
                    .or_else(|| Kind::from_path(&path))
                    .ok_or_else(|| Error::UnknownKind(path.clone()))?;
                let io = |cause| Error::Io {
                    path: path.clone(),
                    cause,
                };
                let meta = std::fs::metadata(&path).map_err(io)?;
                let absolute = std::fs::canonicalize(&path).map_err(io)?;
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| Timestamp::try_from(t).ok())
                    .map(|t| stamp(&t.to_zoned(now.time_zone().clone())))
                    .unwrap_or_else(|| "unknown".to_string());
                let size = meta.len();
                let (content, holds) = match kind {
                    Kind::Msa => {
                        let alignment = Alignment::load(&path)?;
                        let shape = alignment.shape();
                        (Content::Msa(Box::new(alignment)), shape)
                    }
                    Kind::Tree | Kind::Bam => (Content::Unloaded, format!("{size} bytes")),
                };
                let message = format!("opened {} ({}, {holds})", path.display(), kind.as_str());
                self.datasets.push(Dataset {
                    path,
                    kind,
                    size,
                    content,
                });
                self.current = Some(self.datasets.len() - 1);
                Ok(Applied {
                    message,
                    note: Some(format!(
                        "input {} size={size} mtime={modified}",
                        absolute.display()
                    )),
                    created: None,
                })
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
}
