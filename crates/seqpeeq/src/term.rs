//! Terminal set-up, the event loop, and putting the terminal back afterwards.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use ratatui::crossterm::execute;

use crate::app::App;
use crate::ui;
use crate::worker::{Jobs, Worker};

/// How long to wait for input before redrawing while work is running, so
/// that its progress is seen to move.
const TICK: Duration = Duration::from_millis(50);

/// How long to wait for input otherwise. The loop still has to wake up to
/// notice a signal.
const IDLE: Duration = Duration::from_millis(250);

/// How often the watchdog looks at the terminal.
const CHECK: Duration = Duration::from_millis(250);

/// How long after the terminal hangs up the event loop has to leave by
/// itself before the watchdog ends the process.
const GRACE: Duration = Duration::from_secs(1);

/// How long the watchdog waits for cancelled jobs to clean up.
const JOBS_LIMIT: Duration = Duration::from_secs(10);

/// The exit status after a hang-up: 128 + SIGHUP, as if killed by it.
const HANGUP_STATUS: i32 = 129;

/// Notices the signals that ask the program to end: SIGTERM (a cluster's
/// time limit, `kill`) and SIGHUP (the terminal or SSH session closing).
/// They set a flag instead of ending the process, so the loop can leave in
/// the ordinary way and put the terminal back.
pub struct Signals {
    stop: Arc<AtomicBool>,
}

impl Signals {
    pub fn install() -> io::Result<Signals> {
        let stop = Arc::new(AtomicBool::new(false));
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
            signal_hook::flag::register(signal, Arc::clone(&stop))?;
        }
        Ok(Signals { stop })
    }

    /// Whether one of the signals has arrived.
    pub fn stop(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The flag the signals set, for others to set too.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }
}

/// Whether the terminal on `fd` has hung up: its window or SSH session is
/// gone. Asks without waiting and reads nothing. macOS reports the hang-up
/// only to a caller asking whether input is waiting, so that is asked too;
/// input alone means nothing here.
fn terminal_gone(fd: BorrowedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, and `fd` is open for the call.
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    ready > 0 && poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Ends the process when its terminal has hung up and the event loop has
/// not left by itself.
///
/// crossterm 0.29 reads a hung-up terminal forever: every read returns end
/// of input, and its loop only stops on "nothing yet" or an error. The
/// event loop is then stuck inside `event::poll` and never sees the stop
/// flag, so the process would run at full CPU until killed. The watchdog
/// sets the flag on a hang-up, gives the loop [`GRACE`] to leave the
/// ordinary way, then cancels the jobs, lets them clean up and exits.
///
/// A signal with the terminal still there is left to the event loop, which
/// sees it within [`IDLE`] and puts the terminal back.
struct Watchdog {
    done: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Watchdog {
    fn start(stop: Arc<AtomicBool>, jobs: Jobs) -> Watchdog {
        let done = Arc::new(AtomicBool::new(false));
        let standing_down = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            let mut gone_since: Option<Instant> = None;
            loop {
                std::thread::park_timeout(CHECK);
                if standing_down.load(Ordering::SeqCst) {
                    return;
                }
                if !terminal_gone(io::stdin().as_fd()) {
                    continue;
                }
                stop.store(true, Ordering::SeqCst);
                if gone_since.get_or_insert_with(Instant::now).elapsed() < GRACE {
                    continue;
                }
                jobs.cancel_and_wait(JOBS_LIMIT);
                // The loop may have left after all, and is shutting down.
                if standing_down.load(Ordering::SeqCst) {
                    return;
                }
                std::process::exit(HANGUP_STATUS);
            }
        });
        Watchdog { done, thread }
    }

    /// The event loop has left: stop watching.
    fn stand_down(self) {
        self.done.store(true, Ordering::SeqCst);
        self.thread.thread().unpark();
        let _ = self.thread.join();
    }
}

/// Run the interface until the user quits or the process is told to end.
///
/// The terminal is restored on return, on panic and after SIGTERM or
/// SIGHUP: pasting and mouse reporting off, alternate screen left, raw mode
/// off. Without the mouse step the shell would print escape codes on every
/// mouse movement afterwards.
pub fn run(app: &mut App) -> io::Result<()> {
    let signals = Signals::install()?;
    let mut terminal = ratatui::try_init()?;
    // `try_init` installed a panic hook that restores the screen; wrap it so
    // the modes switched on below are switched off first.
    let restore_screen = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        restore_screen(info);
    }));

    let result = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .and_then(|()| event_loop(&mut terminal, app, &signals));

    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, signals: &Signals) -> io::Result<()> {
    let mut worker = Worker::new();
    let watchdog = Watchdog::start(signals.flag(), worker.jobs());
    let result = turns(terminal, app, signals, &mut worker);
    watchdog.stand_down();
    // Do not leave a thread writing a file behind: ask it to stop, and wait.
    app.cancel_jobs();
    worker.join();
    result
}

fn turns(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    signals: &Signals,
    worker: &mut Worker,
) -> io::Result<()> {
    while !app.quit && !signals.stop() {
        app.resize(terminal.size()?);
        app.pump();
        for request in app.outbox.drain(..) {
            worker.submit(request);
        }
        terminal.draw(|frame| ui::draw(frame, app))?;

        let wait = if app.wants_tick() { TICK } else { IDLE };
        if event::poll(wait)? {
            // Take everything that is already waiting (a drag or a paste
            // arrives as a burst) before drawing again.
            loop {
                app.handle_event(&event::read()?);
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        while let Some(done) = worker.poll() {
            app.handle_done(done);
        }
        app.tick(std::time::Instant::now());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_is_gone_once_its_other_end_closes() {
        use std::os::fd::{AsFd, FromRawFd, OwnedFd};
        let (mut master, mut slave) = (0, 0);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "{}", io::Error::last_os_error());
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        assert!(!terminal_gone(slave.as_fd()));
        drop(master);
        assert!(terminal_gone(slave.as_fd()));
    }

    #[test]
    fn a_termination_signal_sets_the_stop_flag_instead_of_killing() {
        let signals = Signals::install().unwrap();
        assert!(!signals.stop());
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        assert!(signals.stop());
    }
}
