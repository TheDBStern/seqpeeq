//! Re-running a session log without an interface.

use crate::log::{FOLLOWS_TREES, MATCHES_IDS};
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
///
/// A log whose header lacks [`FOLLOWS_TREES`] was written before a linked
/// alignment's rows followed their tree, so they do not follow it in the
/// replay either: its row numbers then name the sequences they named when
/// it was written. In the same way, a log whose header lacks
/// [`MATCHES_IDS`] matches tips to rows by whole name only. Scripts with no
/// header follow the rules of today.
pub fn replay(text: &str, session: &mut Session) -> Result<ReplayReport> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let header = || {
        lines
            .iter()
            .take_while(|line| line.is_empty() || line.starts_with('#'))
    };
    if header().any(|line| line.starts_with("# seqpeeq "))
        && !header().any(|&line| line == FOLLOWS_TREES)
    {
        session.set_follow_trees(false);
    }
    if header().any(|line| line.starts_with("# seqpeeq "))
        && !header().any(|&line| line == MATCHES_IDS)
    {
        session.set_match_ids(false);
    }
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

    /// Replay `header`, then: open the alignment and its tree, and delete
    /// row 1. Returns the names left.
    fn linked_replay(header: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let (fa, nwk) = (dir.path().join("s.fa"), dir.path().join("t.nwk"));
        fs::write(&fa, crate::msa::testing::SORTABLE).unwrap();
        fs::write(&nwk, crate::msa::testing::SORTABLE_TREE).unwrap();
        let mut s = Session::new(None);
        let text = format!(
            "{header}open {}\nopen {}\nuse 1\nmsa delete-rows --rows 1\n",
            fa.display(),
            nwk.display()
        );
        replay(&text, &mut s).unwrap();
        let alignment = s.datasets()[0].msa().unwrap();
        (0..alignment.rows())
            .map(|row| alignment.name(row).to_string())
            .collect()
    }

    #[test]
    fn older_logs_keep_rows_in_file_order() {
        let old = "# seqpeeq 0.3.0\n# started 2026-11-01T10:00:00-08:00\n# cwd /x\n";
        assert_eq!(
            linked_replay(old),
            ["r2", "r3"],
            "r1 was row 1 in file order"
        );
    }

    #[test]
    fn newer_logs_and_scripts_follow_the_tree() {
        let new = "# seqpeeq 0.3.0\n# started 2026-11-23T10:00:00-08:00\n# cwd /x\n\
                   # rows follow linked trees\n";
        assert_eq!(
            linked_replay(new),
            ["r3", "r1"],
            "r2 was row 1 in tree order"
        );
        assert_eq!(linked_replay(""), ["r3", "r1"], "a script with no header");
    }

    #[test]
    fn replaying_an_older_log_writes_an_older_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("again.log");
        let mut s = Session::new(Some(crate::SessionLog::new(log.clone())));
        let fa = dir.path().join("s.fa");
        fs::write(&fa, crate::msa::testing::SORTABLE).unwrap();
        replay(&format!("# seqpeeq 0.3.0\nopen {}\n", fa.display()), &mut s).unwrap();
        drop(s);
        let text = fs::read_to_string(&log).unwrap();
        assert!(!text.contains(FOLLOWS_TREES), "{text}");
    }

    /// Replay `log` (its files in `dir`: g.fa with described headers and
    /// g.nwk) then delete row 1: the names left.
    fn left_after_deleting_row_1(header: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let (fa, nwk) = (dir.path().join("g.fa"), dir.path().join("g.nwk"));
        fs::write(&fa, ">A one\nAC\n>B two\nAC\n>C three\nAC\n").unwrap();
        fs::write(&nwk, "((C,B),A);").unwrap();
        let mut s = Session::new(None);
        let text = format!(
            "{header}open {}\nopen {}\nuse 1\nmsa delete-rows --rows 1\n",
            fa.display(),
            nwk.display()
        );
        replay(&text, &mut s).unwrap();
        let a = s.datasets()[0].msa().unwrap();
        (0..a.rows()).map(|row| a.name(row).to_string()).collect()
    }

    #[test]
    fn an_older_log_matches_tips_by_whole_name_only() {
        // 0.4.0: no tip matched `A one`, so nothing was linked and row 1
        // was `A one`.
        let older = format!("# seqpeeq 0.4.0\n{FOLLOWS_TREES}\n");
        assert_eq!(left_after_deleting_row_1(&older), ["B two", "C three"]);
        let now = format!("# seqpeeq 0.4.0\n{FOLLOWS_TREES}\n{MATCHES_IDS}\n");
        assert_eq!(left_after_deleting_row_1(&now), ["B two", "A one"]);
        assert_eq!(left_after_deleting_row_1(""), ["B two", "A one"]);
    }

    #[test]
    fn a_log_says_tips_match_by_id_unless_replaying_an_older_one() {
        let dir = tempfile::tempdir().unwrap();
        let fa = dir.path().join("s.fa");
        fs::write(&fa, crate::msa::testing::SORTABLE).unwrap();
        for (header, says) in [
            ("", true),
            ("# seqpeeq 0.4.0\n# rows follow linked trees\n", false),
        ] {
            let log = dir.path().join(format!("{says}.log"));
            let mut s = Session::new(Some(crate::SessionLog::new(log.clone())));
            replay(&format!("{header}open {}\n", fa.display()), &mut s).unwrap();
            drop(s);
            let text = fs::read_to_string(&log).unwrap();
            assert_eq!(text.contains(MATCHES_IDS), says, "{text}");
        }
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
