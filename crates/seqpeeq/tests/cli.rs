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

#[test]
fn overwrite_is_logged_and_replayed() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("A"), root.path().join("B"));
    for dir in [&a, &b] {
        fs::create_dir(dir).unwrap();
        fs::write(dir.join("in.fa"), TRIO).unwrap();
    }
    let out = seqpeeq(
        &a,
        &[
            "run",
            "--log",
            "s.log",
            "open in.fa",
            "msa overwrite --row 1 --col 1 --seq=-n",
            "msa extract --out o.fa",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        commands(&a.join("s.log"))[1],
        "msa overwrite --row 1 --col 1 --seq=-n"
    );
    assert!(seqpeeq(&b, &["replay", "../A/s.log"]).status.success());
    let written = fs::read_to_string(b.join("o.fa")).unwrap();
    assert!(written.starts_with(">s1\n-nGT-ACGTA\n"), "{written}");
    assert_eq!(written, fs::read_to_string(a.join("o.fa")).unwrap());
}

#[test]
fn insert_and_remove_are_logged_and_replayed() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("A"), root.path().join("B"));
    for dir in [&a, &b] {
        fs::create_dir(dir).unwrap();
        fs::write(dir.join("in.fa"), TRIO).unwrap();
    }
    let out = seqpeeq(
        &a,
        &[
            "run",
            "--log",
            "s.log",
            "open in.fa",
            "msa insert --row 2 --col 10 --seq GG",
            "msa remove --row 3 --col 5 --count 2",
            "msa extract --out o.fa",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        commands(&a.join("s.log"))[1..3],
        [
            "msa insert --row 2 --col 10 --seq GG",
            "msa remove --row 3 --col 5 --count 2"
        ]
    );
    assert!(seqpeeq(&b, &["replay", "../A/s.log"]).status.success());
    let written = fs::read_to_string(a.join("o.fa")).unwrap();
    assert_eq!(
        written,
        ">s1\nACGT-ACGTA--\n>s2\nACGTTACG-GGA\n>s3 third one\nAC--CGTA----\n"
    );
    assert_eq!(fs::read_to_string(b.join("o.fa")).unwrap(), written);
}

#[test]
fn close_makes_another_file_current_on_replay() {
    let dir = setup();
    fs::write(
        dir.path().join("s.log"),
        "open a.fa\nopen t.nwk\nclose\nmsa extract --out o.fa\n",
    )
    .unwrap();
    let out = seqpeeq(dir.path(), &["replay", "s.log"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        fs::read_to_string(dir.path().join("o.fa")).unwrap(),
        ">a\nACGT\n"
    );
}

#[test]
fn run_reads_every_command_before_running_any() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["run", "--log", "s.log", "open a.fa", "bogus"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "", "the first command did not run");
    assert!(!dir.path().join("s.log").exists());
}

#[test]
fn the_log_flag_may_come_before_the_subcommand() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["--log", "one.log", "run", "open a.fa"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(commands(&dir.path().join("one.log")), ["open a.fa"]);
    let out = seqpeeq(dir.path(), &["--log", "two.log", "replay", "one.log"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(commands(&dir.path().join("two.log")), ["open a.fa"]);
    // Files and a subcommand together are still an error, and nothing runs.
    let mixed = seqpeeq(
        dir.path(),
        &["a.fa", "run", "--log", "three.log", "open a.fa"],
    );
    assert!(!mixed.status.success());
    assert_eq!(stdout(&mixed), "");
    assert!(!dir.path().join("three.log").exists());
}

#[test]
fn an_unknown_kind_at_start_fails_before_the_interface() {
    let dir = setup();
    fs::write(dir.path().join("notes.txt"), "x").unwrap();
    let out = seqpeeq(dir.path(), &["notes.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("add --kind"), "{}", stderr(&out));
    assert!(log_files(dir.path()).is_empty());
}

#[test]
fn a_tree_session_replays_to_the_same_files() {
    // Quoted names, a reroot by --node, a rename with a comma, an extract
    // with sequences, undo, and a gzipped save.
    const TREE: &str = "(('Homo sapiens (human)':0.011,'Pan troglodytes, chimp':0.013)98:0.02,\
('Mus musculus':0.08,'Rattus norvegicus':0.09)71:0.03,'It''s a fish':0.4,unquoted_name)root;\n";
    const SEQS: &str = ">Homo sapiens (human)\nACGT\n>Pan troglodytes, chimp\nACGA\n\
>Mus musculus\nTCGA\n>Rattus norvegicus\nTCGG\n>It's a fish\nGGGG\n>unquoted_name\nCCCC\n";
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("A"), root.path().join("B"));
    for dir in [&a, &b] {
        fs::create_dir(dir).unwrap();
        fs::write(dir.join("q.nwk"), TREE).unwrap();
        fs::write(dir.join("q.fa"), SEQS).unwrap();
    }
    let session = seqpeeq(
        &a,
        &[
            "run",
            "--log",
            "s.log",
            "open q.fa",
            "open q.nwk",
            "tree link q.fa",
            "tree reroot --node 3",
            "tree rename --tip \"It's a fish\" --to 'Fish, one'",
            "tree extract --mrca 'Mus musculus' --mrca 'Fish, one' --out sub.nwk --seqs sub.fa",
            "tree prune --tip unquoted_name",
            "undo",
            "tree ladderize --reverse",
            "tree save -o all.nwk.gz",
        ],
    );
    assert!(session.status.success(), "{}", stderr(&session));
    let sub = fs::read_to_string(a.join("sub.nwk")).unwrap();
    assert!(
        sub.contains("'Fish, one'") && sub.contains("'Mus musculus'"),
        "{sub}"
    );
    let fasta = fs::read_to_string(a.join("sub.fa")).unwrap();
    assert!(
        fasta.contains(">Mus musculus\nTCGA\n") && !fasta.contains("Fish"),
        "{fasta}"
    );
    let all = gunzip(&fs::read(a.join("all.nwk.gz")).unwrap());
    assert!(
        all.contains("unquoted_name") && all.contains("'Pan troglodytes, chimp'"),
        "{all}"
    );
    assert_eq!(
        fs::read_to_string(a.join("q.nwk")).unwrap(),
        TREE,
        "the input is untouched"
    );

    let replayed = seqpeeq(&b, &["replay", "../A/s.log"]);
    assert!(replayed.status.success(), "{}", stderr(&replayed));
    assert_eq!(stdout(&replayed).trim(), "replayed 10 commands");
    for file in ["sub.nwk", "sub.fa", "all.nwk.gz"] {
        assert_eq!(
            fs::read(a.join(file)).unwrap(),
            fs::read(b.join(file)).unwrap(),
            "{file}"
        );
    }
}

#[test]
fn run_prints_a_commands_help_and_runs_nothing() {
    let dir = setup();
    let out = seqpeeq(
        dir.path(),
        &["run", "--log", "x.log", "msa fill -h", "open a.fa"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("msa fill [rows] [--cols COLS] --with CHAR"),
        "{text}"
    );
    assert!(!text.contains("opened"), "{text}");
    assert!(!dir.path().join("x.log").exists());
}

#[test]
fn run_help_for_an_unknown_command_fails() {
    let dir = setup();
    let out = seqpeeq(dir.path(), &["run", "msa foo -h"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no command 'msa foo'"),
        "{}",
        stderr(&out)
    );
}

/// The `seqpeeq run` lines in the README's Examples section.
fn readme_recipes() -> Vec<String> {
    let readme =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let examples = readme
        .split("\n## Examples\n")
        .nth(1)
        .expect("an Examples section")
        .split("\n## ")
        .next()
        .unwrap();
    examples
        .lines()
        .filter_map(|line| line.strip_prefix("seqpeeq run "))
        .map(str::to_string)
        .collect()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn readme_recipes_run() {
    let recipes = readme_recipes();
    assert!(recipes.len() >= 2, "{recipes:?}");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for recipe in recipes {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&examples, &dir.path().join("examples"));
        let script = format!("'{}' run {recipe}", env!("CARGO_BIN_EXE_seqpeeq"));
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{recipe}\n{}", stderr(&out));
        // Each argument is a command; its own words name the outputs.
        let words: Vec<String> = shlex::split(&recipe)
            .expect("a shell line")
            .iter()
            .flat_map(|command| shlex::split(command).unwrap_or_default())
            .collect();
        let outs: Vec<&String> = words
            .windows(2)
            .filter(|pair| pair[0] == "--out")
            .map(|pair| &pair[1])
            .collect();
        assert!(!outs.is_empty(), "{recipe} writes nothing");
        for out in outs {
            let path = dir.path().join(out);
            assert!(path.exists(), "{recipe} wrote no {out}");
            if out.ends_with(".fa") {
                // A showcase shouldn't make a file other tools reject.
                let text = fs::read_to_string(&path).unwrap();
                let names: Vec<&str> = text.lines().filter(|l| l.starts_with('>')).collect();
                let unique: std::collections::HashSet<_> = names.iter().collect();
                assert_eq!(unique.len(), names.len(), "{recipe} wrote duplicate names");
            }
        }
    }
}
