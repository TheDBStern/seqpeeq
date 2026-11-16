//! Terminal set-up, the event loop, and putting the terminal back afterwards.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use ratatui::crossterm::execute;

use crate::app::App;
use crate::ui;
use crate::worker::Worker;

/// How long to wait for input before redrawing while work is running, so
/// that its progress is seen to move.
const TICK: Duration = Duration::from_millis(50);

/// How long to wait for input otherwise. The loop still has to wake up to
/// notice a signal.
const IDLE: Duration = Duration::from_millis(250);

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
    let result = turns(terminal, app, signals, &mut worker);
    // Do not leave a thread writing a file behind: ask it to stop, and wait.
    if let Some(busy) = &app.busy {
        busy.progress.cancel();
    }
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_termination_signal_sets_the_stop_flag_instead_of_killing() {
        let signals = Signals::install().unwrap();
        assert!(!signals.stop());
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        assert!(signals.stop());
    }
}
