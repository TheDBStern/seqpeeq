//! Terminal set-up, the event loop, and putting the terminal back afterwards.

use std::io;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;

use crate::app::App;
use crate::ui;

/// Run the interface until the user quits.
///
/// The terminal is restored on return and on panic: raw mode off, alternate
/// screen left, mouse reporting off. Without that last step the shell would
/// print escape codes on every mouse movement after a crash.
pub fn run(app: &mut App) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    // `try_init` installed a panic hook that restores the screen; wrap it so
    // mouse reporting is switched off first.
    let restore_screen = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        restore_screen(info);
    }));

    let result =
        execute!(io::stdout(), EnableMouseCapture).and_then(|()| event_loop(&mut terminal, app));

    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        app.handle_event(&event::read()?);
    }
    Ok(())
}
