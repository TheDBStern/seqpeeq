//! End-to-end tests that run the built `seqpeeq` binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// A temp dir holding `a.fa` and `t.nwk`.
fn setup() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.fa"), ">a\nACGT\n").unwrap();
    fs::write(dir.path().join("t.nwk"), "(a,b);\n").unwrap();
    dir
}

fn seqpeeq(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_seqpeeq"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The command lines of a log file, without comments.
fn commands(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn log_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".log"))
        .collect()
}

#[test]
fn run_then_replay_reproduces_the_log() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["run", "open a.fa", "open t.nwk", "--log", "one.log"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let out = seqpeeq(dir.path(), &["replay", "one.log", "--log", "two.log"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "replayed 2 commands\n");
    assert_eq!(stderr(&out), "");

    let one = commands(&dir.path().join("one.log"));
    assert_eq!(one, ["open a.fa", "open t.nwk"]);
    assert_eq!(one, commands(&dir.path().join("two.log")));
}

#[test]
fn run_without_log_flag_writes_no_log() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["run", "open a.fa"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(log_files(dir.path()).is_empty());
}

#[test]
fn failure_exits_1_with_message() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["run", "open missing.fa"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("seqpeeq: "), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

#[test]
fn replay_reports_line_number() {
    let dir = setup();
    fs::write(dir.path().join("bad.log"), "open a.fa\nbogus\n").unwrap();
    let out = seqpeeq(dir.path(), &["replay", "bad.log"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("line 2"), "{}", stderr(&out));
}

#[test]
fn replay_prints_size_warnings() {
    let dir = setup();
    fs::write(dir.path().join("old.log"), "open a.fa\n# input /x/a.fa size=999 mtime=x\n").unwrap();
    let out = seqpeeq(dir.path(), &["replay", "old.log"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("but was 999"), "{}", stderr(&out));
}

#[test]
fn missing_file_argument_fails_before_ui() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["missing.fa"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("seqpeeq: "), "{}", stderr(&out));
    assert!(!stdout(&out).contains('\x1b'));
    assert!(log_files(dir.path()).is_empty(), "a failed start must not leave a log behind");
}
