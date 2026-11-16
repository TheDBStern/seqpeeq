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
    let out = seqpeeq(
        dir.path(),
        &["run", "open a.fa", "open t.nwk", "--log", "one.log"],
    );
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
    fs::write(
        dir.path().join("old.log"),
        "open a.fa\n# input /x/a.fa size=999 mtime=x\n",
    )
    .unwrap();
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
    assert!(
        log_files(dir.path()).is_empty(),
        "a failed start must not leave a log behind"
    );
}

#[test]
fn interface_without_a_terminal_fails_cleanly() {
    // The test harness captures stdout, so it is not a terminal here.
    let dir = setup();
    let out = seqpeeq(dir.path(), &["a.fa"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "");
    assert!(
        log_files(dir.path()).is_empty(),
        "nothing ran, so there is nothing to log"
    );
}

#[test]
fn an_error_states_its_cause_once() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["run", "open missing.fa"]);
    assert_eq!(
        stderr(&out),
        "seqpeeq: missing.fa: No such file or directory (os error 2)\n"
    );

    fs::write(dir.path().join("bad.log"), "open a.fa\nopen missing.fa\n").unwrap();
    let out = seqpeeq(dir.path(), &["replay", "bad.log"]);
    assert_eq!(
        stderr(&out),
        "seqpeeq: line 2: missing.fa: No such file or directory (os error 2)\n"
    );
}

const TRIO: &str = ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n";

fn gzip(text: &str) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

fn gunzip(bytes: &[u8]) -> String {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// A logged session, replayed in another directory holding the same inputs,
/// writes the same bytes; and replaying never replaces a file.
#[test]
fn replay_reproduces_outputs_byte_for_byte() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("A"), root.path().join("B"));
    let compressed = gzip(TRIO);
    for dir in [&a, &b] {
        fs::create_dir(dir).unwrap();
        fs::write(dir.join("in.fa"), TRIO).unwrap();
        fs::write(dir.join("in.fa.gz"), &compressed).unwrap();
    }
    let inputs_untouched = |dir: &Path| {
        assert_eq!(fs::read_to_string(dir.join("in.fa")).unwrap(), TRIO);
        assert_eq!(fs::read(dir.join("in.fa.gz")).unwrap(), compressed);
    };
    let outputs = ["full.fa", "part.fa.gz", "copy.fa", "two.fa"];
    let contents = |dir: &Path| -> Vec<Vec<u8>> {
        outputs
            .iter()
            .map(|name| fs::read(dir.join(name)).unwrap())
            .collect()
    };

    let session = seqpeeq(
        &a,
        &[
            "run",
            "--log",
            "s.log",
            "open in.fa",
            "msa delete-cols --cols 2-3",
            "msa rename --regex '^s' --replace seq_",
            "msa fill --rows 1 --cols 1-2 --with N",
            "msa delete-rows --match third",
            "undo",
            "msa extract --out full.fa",
            "msa extract --rows 2 --cols 1-4 --wrap 3 --out part.fa.gz",
            "open in.fa.gz",
            "msa extract --out copy.fa",
            "use 1",
            "redo",
            "msa extract --out two.fa",
        ],
    );
    assert!(session.status.success(), "{}", stderr(&session));
    assert_eq!(
        fs::read_to_string(a.join("full.fa")).unwrap(),
        ">seq_1\nNN-ACGTA\n>seq_2\nATTACG-A\n>seq_3 third one\nA-TACGTA\n"
    );
    assert_eq!(
        gunzip(&fs::read(a.join("part.fa.gz")).unwrap()),
        ">seq_2\nATT\nA\n"
    );
    assert_eq!(fs::read_to_string(a.join("copy.fa")).unwrap(), TRIO);
    assert_eq!(
        fs::read_to_string(a.join("two.fa")).unwrap(),
        ">seq_1\nNN-ACGTA\n>seq_2\nATTACG-A\n"
    );
    inputs_untouched(&a);

    let replayed = seqpeeq(&b, &["replay", "../A/s.log"]);
    assert!(replayed.status.success(), "{}", stderr(&replayed));
    assert_eq!(stdout(&replayed).trim(), "replayed 13 commands");
    assert_eq!(contents(&a), contents(&b));
    inputs_untouched(&b);

    // A second replay would have to replace the outputs, so it is refused.
    let again = seqpeeq(&b, &["replay", "../A/s.log"]);
    assert_eq!(again.status.code(), Some(1));
    let message = stderr(&again);
    assert!(
        message.contains("line ") && message.contains("already exists"),
        "{message}"
    );
    assert_eq!(contents(&a), contents(&b));
    inputs_untouched(&b);
}
