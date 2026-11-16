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
