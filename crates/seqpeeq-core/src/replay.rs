//! Re-running a session log without an interface.

use crate::{Command, Error, Result, Session};

/// What a replay did.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayReport {
    /// How many commands were executed.
    pub commands: usize,
    /// Signs that the inputs are not the ones the log was written against.
    pub warnings: Vec<String>,
}

/// Execute every command line of a log, in order, stopping at the first failure.
///
/// Lines are trimmed, so Windows line endings are fine; blank lines and `#`
/// comments are skipped. An `open` followed by its `# input … size=N` comment
/// is checked against the file's current size. Only size is compared, because
/// copying a file between machines changes its modification time.
pub fn replay(text: &str, session: &mut Session) -> Result<ReplayReport> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut report = ReplayReport {
        commands: 0,
        warnings: Vec::new(),
    };
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let at_line = |cause| Error::Replay {
            line: number,
            cause: Box::new(cause),
        };
        let cmd = Command::parse_line(line).map_err(at_line)?;
        let opened_before = session.datasets().len();
        session.execute(cmd).map_err(at_line)?;
        report.commands += 1;

        let recorded = lines.get(index + 1).and_then(|next| recorded_size(next));
        if let (Some(was), Some(dataset)) = (recorded, session.datasets().get(opened_before))
            && dataset.size != was
        {
            report.warnings.push(format!(
                "line {number}: {} is {} bytes but was {was} when the log was written",
                dataset.path.display(),
                dataset.size
            ));
        }
    }
    Ok(report)
}

/// The size recorded in a `# input <path> size=<n> mtime=<t>` comment.
fn recorded_size(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("# input ")?;
    // The path may itself contain " size=", so take the last one.
    let after = &rest[rest.rfind(" size=")? + " size=".len()..];
    after.split(' ').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, Session};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A temp dir holding 4-byte `a.fa` and `b.fa`.
    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.fa"), dir.path().join("b.fa"));
        fs::write(&a, ">a\nA").unwrap();
        fs::write(&b, ">b\nC").unwrap();
        (dir, a, b)
    }

    #[test]
    fn replays_and_tolerates_crlf() {
        let (_dir, a, b) = setup();
        let mut s = Session::new(None);
        let text = format!(
            "# seqpeeq 0.1.0\r\n\r\nopen {}  \r\nopen {}\r\n",
            a.display(),
            b.display()
        );
        let r = replay(&text, &mut s).unwrap();
        assert_eq!((r.commands, s.datasets().len()), (2, 2));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn stops_at_bad_line() {
        let (_dir, a, b) = setup();
        let mut s = Session::new(None);
        let text = format!("open {}\nbogus\nopen {}\n", a.display(), b.display());
        let Err(Error::Replay { line, .. }) = replay(&text, &mut s) else {
            panic!()
        };
        assert_eq!((line, s.datasets().len()), (2, 1));
    }

    #[test]
    fn warns_when_input_size_changed() {
        let (_dir, a, _b) = setup();
        let mut s = Session::new(None);
        let text = format!(
            "open {}\n# input {} size=999 mtime=2026-11-01T10:00:00-08:00\n",
            a.display(),
            a.display()
        );
        let r = replay(&text, &mut s).unwrap();
        assert_eq!(r.warnings.len(), 1);
        assert!(
            r.warnings[0].starts_with("line 1: ")
                && r.warnings[0].contains("is 4 bytes but was 999"),
            "{}",
            r.warnings[0]
        );
    }

    #[test]
    fn matching_input_size_gives_no_warning() {
        let (_dir, a, _b) = setup();
        let mut s = Session::new(None);
        let text = format!(
            "open {}\n# input {} size=4 mtime=x\n",
            a.display(),
            a.display()
        );
        assert!(replay(&text, &mut s).unwrap().warnings.is_empty());
    }
}
