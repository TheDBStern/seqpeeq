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

/// Execute every command line of a log, in order.
///
/// Every line is parsed before anything is executed, so a log with a line
/// that cannot be understood changes nothing. A command that parses but then
/// fails stops the replay there; the commands before it have taken effect.
///
/// Lines are trimmed, so Windows line endings are fine; blank lines, `#`
/// comments and a leading byte-order mark are skipped. An `open` followed by
/// its `# input … size=N` comment is checked against the file's current size.
/// Only size is compared, because copying a file between machines changes its
/// modification time.
pub fn replay(text: &str, session: &mut Session) -> Result<ReplayReport> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let at_line = |index: usize| {
        move |cause| Error::Replay {
            line: index + 1,
            cause: Box::new(cause),
        }
    };

    let mut commands = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        commands.push((index, Command::parse_line(line).map_err(at_line(index))?));
    }

    let mut report = ReplayReport {
        commands: 0,
        warnings: Vec::new(),
    };
    for (index, cmd) in commands {
        let opened_before = session.datasets().len();
        session.execute(cmd).map_err(at_line(index))?;
        report.commands += 1;

        let recorded = lines.get(index + 1).and_then(|next| recorded_size(next));
        if let (Some(was), Some(dataset)) = (recorded, session.datasets().get(opened_before))
            && dataset.size != was
        {
            report.warnings.push(format!(
                "line {}: {} is {} bytes but was {was} when the log was written",
                index + 1,
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

    /// A temp dir holding the ten-column TRIO alignment.
    fn trio() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.fa");
        fs::write(&path, crate::msa::testing::TRIO).unwrap();
        (dir, path)
    }

    #[test]
    fn nothing_runs_when_a_later_line_does_not_parse() {
        let (_dir, a) = trio();
        let mut s = Session::new(None);
        let text = format!(
            "open {}\nmsa delete-cols --cols 1\nmsa frobnicate\n",
            a.display()
        );
        let Err(Error::Replay { line, .. }) = replay(&text, &mut s) else {
            panic!()
        };
        assert_eq!(line, 3);
        assert!(s.datasets().is_empty() && s.history().is_empty());
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let (_dir, a) = trio();
        let mut s = Session::new(None);
        let text = format!("\u{feff}open {}\n", a.display());
        assert_eq!(replay(&text, &mut s).unwrap().commands, 1);
    }

    #[test]
    fn a_command_that_fails_while_running_reports_its_line_and_keeps_earlier_work() {
        let (_dir, a) = trio();
        let mut s = Session::new(None);
        let text = format!("open {}\n# note\nmsa delete-cols --cols 99\n", a.display());
        let error = replay(&text, &mut s).unwrap_err();
        assert!(matches!(error, Error::Replay { line: 3, .. }));
        assert_eq!(
            error.to_string(),
            "line 3: column 99 is past the last column (10)"
        );
        assert_eq!(s.datasets().len(), 1);
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
