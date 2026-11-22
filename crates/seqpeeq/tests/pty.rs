//! The real `seqpeeq` binary, run in a pseudo-terminal.
//!
//! ratatui redraws only the cells that changed, so text is looked for on an
//! emulated screen, never in the stream of bytes the program wrote.

use std::io::{Read, Write};
use std::num::NonZero;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, ExitStatus, PtySize, native_pty_system};
use tempfile::TempDir;

const TRIO: &str = ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n";
const TREE: &str = "((A:1,B:2)90:3,(C:2,(D:1,E:1):1)70:1,F:0.5);\n";
const PATIENCE: Duration = Duration::from_secs(10);

/// Everything the program has written, raw and as a screen.
struct Seen {
    raw: Vec<u8>,
    screen: vt100::Parser,
}

struct Pty {
    dir: TempDir,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    seen: Arc<Mutex<Seen>>,
    // Kept open so the program's terminal stays there until the test ends.
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Pty {
    /// Run `seqpeeq <args>` in a new temp dir that holds `a.fa`.
    fn start(args: &[&str]) -> Pty {
        Pty::start_with(args, |_| {})
    }

    /// Run `seqpeeq <args>` in a new temp dir that holds `a.fa` and
    /// whatever `prepare` puts there.
    fn start_with(args: &[&str], prepare: impl FnOnce(&Path)) -> Pty {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.fa"), TRIO).unwrap();
        std::fs::write(dir.path().join("t.nwk"), TREE).unwrap();
        prepare(dir.path());
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system().openpty(size).unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_seqpeeq"));
        command.args(args);
        command.cwd(dir.path());
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);

        let seen = Arc::new(Mutex::new(Seen {
            raw: Vec::new(),
            screen: vt100::Parser::new(NonZero::new(24).unwrap(), NonZero::new(80).unwrap(), 0),
        }));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let sink = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(read) = reader.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let mut seen = sink.lock().unwrap();
                seen.raw.extend_from_slice(&chunk[..read]);
                seen.screen.process(&chunk[..read]);
            }
        });
        Pty {
            dir,
            child,
            writer: pair.master.take_writer().unwrap(),
            seen,
            _master: pair.master,
        }
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }

    fn pid(&self) -> u32 {
        self.child.process_id().unwrap()
    }

    fn screen(&self) -> String {
        self.seen.lock().unwrap().screen.screen().contents()
    }

    /// Wait until `text` is on the screen.
    fn wait_for(&self, text: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self.screen().contains(text) {
            assert!(
                Instant::now() < deadline,
                "{text:?} never appeared; the screen shows:\n{}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait until every one of `texts` is on the screen at once.
    fn wait_for_all(&self, texts: &[&str]) {
        let deadline = Instant::now() + PATIENCE;
        while !texts.iter().all(|text| self.screen().contains(text)) {
            assert!(
                Instant::now() < deadline,
                "{texts:?} never appeared together; the screen shows:\n{}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// Wait for the program to end.
    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // Let the reader take in what was written last.
                std::thread::sleep(Duration::from_millis(100));
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the program did not end; the screen shows:\n{}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// What was written after the last `marker`.
    fn output_after(&self, marker: &str) -> String {
        let seen = self.seen.lock().unwrap();
        let raw = String::from_utf8_lossy(&seen.raw).into_owned();
        let at = raw.rfind(marker).expect("the marker was written");
        raw[at..].to_string()
    }

    /// The escape codes that undo what the interface switched on: leave the
    /// alternate screen, stop reporting the mouse, stop bracketing pastes.
    fn assert_terminal_restored(&self, when: &str) {
        let after = self.output_after("\x1b[?1049h");
        for code in ["\x1b[?1049l", "\x1b[?1000l", "\x1b[?2004l"] {
            assert!(after.contains(code), "{when}: {code:?} was not sent");
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn starts_loads_and_quits_leaving_the_terminal_as_it_was() {
    let mut run = Pty::start(&["--no-log", "a.fa"]);
    run.wait_for("opened a.fa");
    run.send("q");
    assert!(run.wait_exit().success());
    run.assert_terminal_restored("after q");
}

#[test]
fn a_signal_puts_the_terminal_back() {
    for signal in ["TERM", "HUP"] {
        let mut run = Pty::start(&["--no-log", "a.fa"]);
        run.wait_for("opened a.fa");
        let killed = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &run.pid().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        run.wait_exit();
        run.assert_terminal_restored(signal);
        assert!(run.dir().join("a.fa").exists());
    }
}

#[test]
fn a_viewer_whose_terminal_closes_ends() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.fa"), TRIO).unwrap();
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system().openpty(size).unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_seqpeeq"));
    command.args(["--no-log", "a.fa"]);
    command.cwd(dir.path());
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);

    // Read until the file is on screen, then let go of the reader: every
    // handle on the terminal's other end must close for it to hang up.
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (drawn, on_screen) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut screen =
            vt100::Parser::new(NonZero::new(24).unwrap(), NonZero::new(80).unwrap(), 0);
        let mut chunk = [0; 4096];
        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 {
                break;
            }
            screen.process(&chunk[..read]);
            if screen.screen().contents().contains("opened a.fa") {
                let _ = drawn.send(());
                return;
            }
        }
    });
    if on_screen.recv_timeout(PATIENCE).is_err() {
        let _ = child.kill();
        panic!("the file never showed");
    }
    drop(pair.master);

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the viewer was still running 5 s after its terminal closed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.exit_code(), 129, "{status:?}");
}

#[test]
fn an_idle_viewer_with_its_terminal_open_keeps_running() {
    let mut run = Pty::start(&["--no-log", "a.fa"]);
    run.wait_for("opened a.fa");
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        run.child.try_wait().unwrap().is_none(),
        "the viewer ended by itself"
    );
}

#[test]
fn the_grid_is_drawn_and_an_edit_reaches_the_log() {
    let mut run = Pty::start(&["--log", "s.log", "a.fa"]);
    run.wait_for("s3 third one");
    run.send("jVd");
    run.wait_for("deleted 1 sequence");
    run.send("q");
    run.wait_for("Quit anyway?");
    run.send("y");
    assert!(run.wait_exit().success());
    run.assert_terminal_restored("after quitting with edits");
    let log = std::fs::read_to_string(run.dir().join("s.log")).unwrap();
    assert!(log.contains("\nmsa delete-rows --rows 2\n"), "{log}");
}

#[test]
fn a_tree_opens_and_is_drawn() {
    let mut run = Pty::start(&["--no-log", "t.nwk"]);
    run.wait_for("6 tips");
    run.wait_for("┤");
    run.wait_for("─ F");
    run.send("j");
    run.wait_for("B · length 2");
    run.send("q");
    assert!(run.wait_exit().success());
    run.assert_terminal_restored("after q on a tree");
}

#[test]
fn a_read_opens_along_its_own_bases() {
    let mut run = Pty::start_with(&["--no-log", "multi.bam"], |dir| {
        seqpeeq_core::bam::testing::multi(dir);
    });
    run.wait_for("g1  2,000 bp");
    run.send("g");
    run.send("g1:101-160\r");
    // `depth N` joins the status line once the reads are read.
    run.wait_for_all(&["g1:101  ", "1 bp a column"]);
    run.send("a");
    run.wait_for("x1 · pair");
    run.wait_for("g1:401 (−)");
    // Esc alone: sent with the next key it would read as Alt-A.
    run.send("\x1b");
    run.wait_for("g1  2,000 bp");
    run.send("A");
    run.wait_for("aligning elsewhere");
    run.send("q");
    assert!(run.wait_exit().success());
    run.assert_terminal_restored("after q over the list");
}

#[test]
fn a_bam_opens_is_drawn_and_jumps() {
    let mut run = Pty::start_with(&["--no-log", "reads.bam", "--ref", "ref.fa"], |dir| {
        seqpeeq_core::bam::testing::fixture(dir);
    });
    run.wait_for("c1  1,000 bp");
    run.send("g");
    run.send("c1:101-160\r");
    // `depth N` joins the status line once the reads are read.
    run.wait_for_all(&["c1:101  ", "1 bp a column"]);
    run.wait_for(&seqpeeq_core::bam::testing::ref_bases("c1", 101, 140));
    run.send("]");
    run.wait_for("c1:301 ");
    run.send("q");
    assert!(run.wait_exit().success());
    run.assert_terminal_restored("after q on a BAM");
}
