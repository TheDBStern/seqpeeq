//! The session log file: one executed command per line, comments start with `#`.
//!
//! The file is created on the first command, so a session that changes
//! nothing leaves nothing behind, and it is created with `create_new`, so an
//! existing log is never overwritten or extended.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use jiff::Zoned;

use crate::{Error, Result};

/// The timestamp form used throughout the log, e.g. `2026-11-15T16:05:03-08:00`.
pub(crate) fn stamp(time: &Zoned) -> String {
    time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// Escape control characters, so text placed in a comment cannot end the
/// comment line and smuggle a command line into the log.
pub(crate) fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

type Output = Box<dyn Write + Send>;
type Opener = Box<dyn FnMut(&Path) -> io::Result<Output> + Send>;

pub struct SessionLog {
    path: PathBuf,
    /// Creates the output on first use.
    opener: Opener,
    out: Option<Output>,
    /// Command lines written so far.
    commands: usize,
    /// Set by a failed write. The file may then end in a partial record, so
    /// nothing more is added to it and the session refuses further commands.
    broken: bool,
}

/// A log that was created but never recorded a command (every command failed)
/// holds only a header, so it is removed rather than left as clutter.
impl Drop for SessionLog {
    fn drop(&mut self) {
        if self.out.take().is_some() && self.commands == 0 {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl SessionLog {
    /// A log that will be written to `path`. Nothing is touched on disk yet.
    /// The file is created with `create_new`: an existing file is an error.
    pub fn new(path: PathBuf) -> SessionLog {
        SessionLog::with_opener(
            path,
            Box::new(|path| {
                let file = OpenOptions::new().write(true).create_new(true).open(path)?;
                Ok(Box::new(file) as Output)
            }),
        )
    }

    /// As [`SessionLog::new`], but the file refuses to grow past `bytes`.
    /// This is for tests and fault drills: it is how a full disk or a quota
    /// looks to the session.
    pub fn with_limit(path: PathBuf, bytes: usize) -> SessionLog {
        struct Limited {
            file: std::fs::File,
            left: usize,
        }
        impl Write for Limited {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if bytes.len() > self.left {
                    return Err(io::Error::other("no space left (a limit was set)"));
                }
                self.left -= bytes.len();
                self.file.write_all(bytes)?;
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.file.flush()
            }
        }
        SessionLog::with_opener(
            path,
            Box::new(move |path| {
                let file = OpenOptions::new().write(true).create_new(true).open(path)?;
                Ok(Box::new(Limited { file, left: bytes }) as Output)
            }),
        )
    }

    /// As [`SessionLog::new`], with the way the output is created supplied.
    pub(crate) fn with_opener(path: PathBuf, opener: Opener) -> SessionLog {
        SessionLog {
            path,
            opener,
            out: None,
            commands: 0,
            broken: false,
        }
    }

    /// `dir/seqpeeq-YYYYMMDD-HHMMSS.log`
    pub fn default_path(dir: &Path, now: &Zoned) -> PathBuf {
        dir.join(format!("seqpeeq-{}.log", now.strftime("%Y%m%d-%H%M%S")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether a write has failed, so that nothing more can be recorded.
    pub(crate) fn is_broken(&self) -> bool {
        self.broken
    }

    /// Create the file and write its header. Does nothing once the file is
    /// open; fails if an earlier write failed.
    pub fn ensure_open(&mut self, now: &Zoned) -> Result<()> {
        if self.broken {
            return Err(self.error(io::Error::other(
                "an earlier write failed, so the log is incomplete",
            )));
        }
        if self.out.is_some() {
            return Ok(());
        }
        let out = (self.opener)(&self.path).map_err(|e| self.error(e))?;
        self.out = Some(out);
        let cwd = match std::env::current_dir() {
            Ok(dir) => dir.display().to_string(),
            Err(_) => "unknown".to_string(),
        };
        self.write(&format!(
            "# seqpeeq {}\n# started {}\n# cwd {}\n",
            env!("CARGO_PKG_VERSION"),
            stamp(now),
            one_line(&cwd)
        ))
    }

    /// Record one executed command: a timestamp comment, the command line and
    /// an optional `# note` comment, written in a single call so a failure
    /// cannot leave a command without its note.
    pub fn record(&mut self, line: &str, note: Option<&str>, now: &Zoned) -> Result<()> {
        self.ensure_open(now)?;
        let mut text = format!("# {}\n{line}\n", stamp(now));
        if let Some(note) = note {
            text.push_str(&format!("# {}\n", one_line(note)));
        }
        self.write(&text)?;
        self.commands += 1;
        Ok(())
    }

    fn write(&mut self, text: &str) -> Result<()> {
        let result = match &mut self.out {
            Some(out) => out.write_all(text.as_bytes()).and_then(|()| out.flush()),
            None => Err(io::Error::other("log file is not open")),
        };
        result.map_err(|e| {
            self.broken = true;
            self.error(e)
        })
    }

    fn error(&self, cause: io::Error) -> Error {
        Error::Log {
            path: self.path.clone(),
            cause,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Error;
    use jiff::Zoned;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// A fixed instant with a fixed offset, so tests need no time-zone database.
    pub(crate) fn fixed() -> Zoned {
        "2026-11-15T16:05:03-08:00[-08:00]".parse().unwrap()
    }

    /// A log whose "file" is a shared buffer that refuses any write that would
    /// take it past `limit` bytes, as a full disk or a quota would.
    pub(crate) fn limited_log(limit: usize) -> (SessionLog, Arc<Mutex<Vec<u8>>>) {
        struct Limited(Arc<Mutex<Vec<u8>>>, usize);
        impl Write for Limited {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let mut buffer = self.0.lock().unwrap();
                if buffer.len() + bytes.len() > self.1 {
                    return Err(io::Error::other("disk full"));
                }
                buffer.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let shared = buffer.clone();
        let log = SessionLog::with_opener(
            PathBuf::from("/nonexistent/test.log"),
            Box::new(move |_| Ok(Box::new(Limited(shared.clone(), limit)))),
        );
        (log, buffer)
    }

    #[test]
    fn control_characters_cannot_start_a_new_log_line() {
        assert_eq!(one_line("proj\nopen secret.nwk"), "proj\\nopen secret.nwk");
        assert_eq!(one_line("a\r\u{1b}b"), "a\\r\\u{1b}b");
        assert_eq!(one_line("配列 ok"), "配列 ok");
    }

    #[test]
    fn a_note_with_a_line_break_stays_one_comment_line() {
        let (mut log, buffer) = limited_log(10_000);
        log.ensure_open(&fixed()).unwrap();
        log.record("open x.fa", Some("input a\nopen evil.fa size=1"), &fixed())
            .unwrap();
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        let commands: Vec<_> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(commands, ["open x.fa"]);
        assert_eq!(text.lines().count(), 6);
    }

    #[test]
    fn a_failed_record_writes_nothing_and_blocks_the_log() {
        let (mut log, buffer) = limited_log(200);
        log.ensure_open(&fixed()).unwrap();
        let header = buffer.lock().unwrap().clone();
        let long = "x".repeat(300);
        assert!(matches!(
            log.record(&long, None, &fixed()),
            Err(Error::Log { .. })
        ));
        assert_eq!(*buffer.lock().unwrap(), header, "no partial record");
        // The log is now incomplete, so nothing more may be added to it.
        assert!(matches!(log.ensure_open(&fixed()), Err(Error::Log { .. })));
        assert!(matches!(
            log.record("open a.fa", None, &fixed()),
            Err(Error::Log { .. })
        ));
        assert_eq!(*buffer.lock().unwrap(), header);
    }

    #[test]
    fn default_log_name() {
        assert_eq!(
            SessionLog::default_path(Path::new("/d"), &fixed()),
            Path::new("/d/seqpeeq-20261115-160503.log")
        );
    }

    #[test]
    fn a_limited_log_refuses_writes_past_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        // The header names the working directory, so measure what one
        // command takes rather than assuming a size.
        let probe = dir.path().join("probe.log");
        let mut log = SessionLog::new(probe.clone());
        log.record("open a.fa", None, &fixed()).unwrap();
        drop(log);
        let used = std::fs::metadata(&probe).unwrap().len() as usize;

        let path = dir.path().join("s.log");
        let mut log = SessionLog::with_limit(path.clone(), used + 50);
        log.record("open a.fa", None, &fixed()).unwrap();
        assert!(!log.is_broken());
        let long = "x".repeat(200);
        assert!(matches!(
            log.record(&long, None, &fixed()),
            Err(Error::Log { .. })
        ));
        assert!(log.is_broken());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("open a.fa\n"), "{text}");
    }
}
