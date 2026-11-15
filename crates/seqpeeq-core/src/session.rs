//! A session: the open datasets, and the one place commands are executed.

use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};

use crate::log::stamp;
use crate::{Command, Error, Kind, Result, SessionLog};

/// A file registered with the session.
#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
}

/// What a successfully executed command reports back to the interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub message: String,
}

pub struct Session {
    datasets: Vec<Dataset>,
    history: Vec<String>,
    log: Option<SessionLog>,
    clock: fn() -> Zoned,
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
        let (outcome, note) = self.apply(cmd, &now)?;
        self.history.push(line.clone());
        if let Some(log) = &mut self.log {
            log.append(&line, &now)?;
            if let Some(note) = note {
                log.comment(&note)?;
            }
        }
        Ok(outcome)
    }

    pub fn datasets(&self) -> &[Dataset] {
        &self.datasets
    }

    /// The text form of every command executed so far, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn log_path(&self) -> Option<&Path> {
        self.log.as_ref().map(SessionLog::path)
    }

    /// Apply a command to the session state. Returns the outcome and an
    /// optional comment to write to the log after the command line.
    fn apply(&mut self, cmd: Command, now: &Zoned) -> Result<(Outcome, Option<String>)> {
        match cmd {
            Command::Open { path, kind } => {
                let kind = kind
                    .or_else(|| Kind::from_path(&path))
                    .ok_or_else(|| Error::UnknownKind(path.clone()))?;
                let io = |source| Error::Io {
                    path: path.clone(),
                    source,
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
                let note = format!("input {} size={size} mtime={modified}", absolute.display());
                let message = format!("opened {} ({}, {size} bytes)", path.display(), kind.as_str());
                self.datasets.push(Dataset { path, kind, size });
                Ok((Outcome { message }, Some(note)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::tests::fixed;
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
        Command::Open { path: path.to_path_buf(), kind: None }
    }

    #[test]
    fn open_registers_and_logs() {
        let (dir, fa) = setup();
        let log = dir.path().join("s.log");
        let mut s = Session::with_clock(Some(SessionLog::new(log.clone())), fixed);
        let out = s.execute(open(&fa)).unwrap();
        assert!(out.message.ends_with("(msa, 4 bytes)"), "{}", out.message);
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
        assert!(lines[5].starts_with("# input /") && lines[5].contains(" size=4 mtime="), "{}", lines[5]);
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
    fn unknown_kind_is_refused() {
        let (dir, _fa) = setup();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "(a,b);").unwrap();
        let mut s = Session::new(None);
        let err = s.execute(open(&notes)).unwrap_err();
        assert!(matches!(err, Error::UnknownKind(_)), "{err}");
        s.execute(Command::Open { path: notes, kind: Some(Kind::Tree) }).unwrap();
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
        assert!(text.contains("--log PATH") && text.contains("--no-log"), "{text}");
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
}
