//! The session log file: one executed command per line, comments start with `#`.
//!
//! The file is created on the first command, so a session that changes
//! nothing leaves nothing behind, and it is created with `create_new`, so an
//! existing log is never overwritten or extended.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use jiff::Zoned;

use crate::{Error, Result};

/// The timestamp form used throughout the log, e.g. `2026-11-15T16:05:03-08:00`.
pub(crate) fn stamp(time: &Zoned) -> String {
    time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub struct SessionLog {
    path: PathBuf,
    file: Option<File>,
    /// Command lines written so far.
    commands: usize,
}

/// A log that was created but never recorded a command (every command failed)
/// holds only a header, so it is removed rather than left as clutter.
impl Drop for SessionLog {
    fn drop(&mut self) {
        if self.file.take().is_some() && self.commands == 0 {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl SessionLog {
    /// A log that will be written to `path`. Nothing is touched on disk yet.
    pub fn new(path: PathBuf) -> SessionLog {
        SessionLog {
            path,
            file: None,
            commands: 0,
        }
    }

    /// `dir/seqpeeq-YYYYMMDD-HHMMSS.log`
    pub fn default_path(dir: &Path, now: &Zoned) -> PathBuf {
        dir.join(format!("seqpeeq-{}.log", now.strftime("%Y%m%d-%H%M%S")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create the file and write its header. Does nothing once the file is open.
    pub fn ensure_open(&mut self, now: &Zoned) -> Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
            .map_err(|e| self.error(e))?;
        self.file = Some(file);
        let cwd = std::env::current_dir().map_err(|e| self.error(e))?;
        self.write(&format!(
            "# seqpeeq {}\n# started {}\n# cwd {}\n",
            env!("CARGO_PKG_VERSION"),
            stamp(now),
            cwd.display()
        ))
    }

    /// Record one executed command, preceded by a timestamp comment.
    pub fn append(&mut self, line: &str, now: &Zoned) -> Result<()> {
        self.ensure_open(now)?;
        self.write(&format!("# {}\n{line}\n", stamp(now)))?;
        self.commands += 1;
        Ok(())
    }

    /// Write a `# text` comment line.
    pub fn comment(&mut self, text: &str) -> Result<()> {
        self.write(&format!("# {text}\n"))
    }

    fn write(&mut self, text: &str) -> Result<()> {
        let result = match &mut self.file {
            Some(file) => file.write_all(text.as_bytes()).and_then(|()| file.flush()),
            None => Err(io::Error::other("log file is not open")),
        };
        result.map_err(|e| self.error(e))
    }

    fn error(&self, source: io::Error) -> Error {
        Error::Log {
            path: self.path.clone(),
            source,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use jiff::Zoned;
    use std::path::Path;

    /// A fixed instant with a fixed offset, so tests need no time-zone database.
    pub(crate) fn fixed() -> Zoned {
        "2026-11-15T16:05:03-08:00[-08:00]".parse().unwrap()
    }

    #[test]
    fn default_log_name() {
        assert_eq!(
            SessionLog::default_path(Path::new("/d"), &fixed()),
            Path::new("/d/seqpeeq-20261115-160503.log")
        );
    }
}
