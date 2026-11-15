//! Interface state and how input events change it. No drawing, no terminal I/O.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use seqpeeq_core::{Command, Session};

use crate::ui;

pub enum Mode {
    Normal,
    /// Typing on the `:` command line; holds the text so far.
    Command(String),
    Help,
}

pub struct App {
    pub session: Session,
    pub mode: Mode,
    /// The last outcome or error, shown on the bottom line.
    pub status: String,
    /// Index of the dataset whose tab is shown.
    pub active: usize,
    pub show_log: bool,
    pub quit: bool,
}

impl App {
    pub fn new(session: Session) -> App {
        App {
            session,
            mode: Mode::Normal,
            status: String::new(),
            active: 0,
            show_log: false,
            quit: false,
        }
    }

    pub fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                self.quit = true;
            }
            return;
        }
        match &mut self.mode {
            Mode::Normal => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char(':') => self.mode = Mode::Command(String::new()),
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Char('L') => self.show_log = !self.show_log,
                KeyCode::Tab => self.step_tab(1),
                KeyCode::BackTab => self.step_tab(-1),
                _ => {}
            },
            Mode::Command(buffer) => match key.code {
                KeyCode::Char(c) => buffer.push(c),
                KeyCode::Backspace => {
                    if buffer.pop().is_none() {
                        self.mode = Mode::Normal;
                    }
                }
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Enter => {
                    let line = std::mem::take(buffer);
                    self.mode = Mode::Normal;
                    self.run_line(&line);
                }
                _ => {}
            },
            Mode::Help => self.mode = Mode::Normal,
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) {
        let left_click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
        if left_click && mouse.row == 0 && matches!(self.mode, Mode::Normal) {
            let ranges = ui::tab_ranges(&ui::tab_titles(self));
            if let Some(tab) = ranges.iter().position(|r| r.contains(&mouse.column)) {
                self.active = tab;
            }
        }
    }

    /// Move `step` tabs right (negative: left), wrapping at the ends.
    fn step_tab(&mut self, step: isize) {
        let count = self.session.datasets().len() as isize;
        if count > 0 {
            self.active = (self.active as isize + step).rem_euclid(count) as usize;
        }
    }

    /// Run one line typed on the command line.
    fn run_line(&mut self, line: &str) {
        let line = line.trim();
        // Leaving is an interface action, not a change to the data: not logged.
        if line == "q" || line == "quit" {
            self.quit = true;
            return;
        }
        let opened_before = self.session.datasets().len();
        let result = Command::parse_line(line).and_then(|cmd| self.session.execute(cmd));
        self.status = match result {
            Ok(outcome) => outcome.message,
            Err(error) => format!("error: {error}"),
        };
        if self.session.datasets().len() > opened_before {
            self.active = self.session.datasets().len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tab_ranges;
    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use seqpeeq_core::Session;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn key(c: char) -> Event {
        press(KeyCode::Char(c))
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_event(&key(c));
        }
    }

    fn enter(app: &mut App) {
        app.handle_event(&press(KeyCode::Enter));
    }

    fn click(column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// A fresh app plus a temp dir holding `a.fa` and `t.nwk`.
    fn setup() -> (App, TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (fa, nwk) = (dir.path().join("a.fa"), dir.path().join("t.nwk"));
        fs::write(&fa, ">a\nA").unwrap();
        fs::write(&nwk, "(a,b);").unwrap();
        (App::new(Session::new(None)), dir, fa, nwk)
    }

    /// An app with both files open; the second is active.
    fn with_two_tabs() -> (App, TempDir) {
        let (mut app, dir, fa, nwk) = setup();
        for path in [fa, nwk] {
            type_str(&mut app, &format!(":open {}", path.display()));
            enter(&mut app);
        }
        (app, dir)
    }

    #[test]
    fn colon_command_opens_file() {
        let (mut app, _dir, fa, _) = setup();
        type_str(&mut app, &format!(":open {}", fa.display()));
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.session.datasets().len(), 1);
        assert!(app.status.starts_with("opened "), "{}", app.status);
    }

    #[test]
    fn bad_command_shows_one_line_error() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":bogus");
        enter(&mut app);
        assert!(
            app.status.starts_with("error: ") && !app.status.contains('\n'),
            "{}",
            app.status
        );
        assert!(app.session.history().is_empty());
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn quit_paths() {
        let (mut app, ..) = setup();
        app.handle_event(&key('q'));
        assert!(app.quit);

        for typed in [":q", ":quit"] {
            let (mut app, ..) = setup();
            type_str(&mut app, typed);
            assert!(!app.quit);
            enter(&mut app);
            assert!(app.quit, "{typed}");
            assert!(app.session.history().is_empty());
        }

        let (mut app, ..) = setup();
        type_str(&mut app, ":open");
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.quit);
    }

    #[test]
    fn q_inside_command_buffer_is_text() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":open q");
        assert!(!app.quit);
        assert!(matches!(&app.mode, Mode::Command(buffer) if buffer == "open q"));
    }

    #[test]
    fn escape_and_backspace_leave_command_mode() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":ab");
        app.handle_event(&press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));

        type_str(&mut app, ":");
        app.handle_event(&press(KeyCode::Backspace));
        assert!(matches!(app.mode, Mode::Normal));

        type_str(&mut app, ":é");
        app.handle_event(&press(KeyCode::Backspace));
        assert!(matches!(&app.mode, Mode::Command(buffer) if buffer.is_empty()));
    }

    #[test]
    fn tabs_cycle_and_wrap() {
        let (mut app, ..) = setup();
        app.handle_event(&press(KeyCode::Tab));
        app.handle_event(&press(KeyCode::BackTab));
        assert_eq!(app.active, 0);

        let (mut app, _dir) = with_two_tabs();
        app.active = 0;
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(app.active, 1);
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(app.active, 0);
        app.handle_event(&press(KeyCode::BackTab));
        assert_eq!(app.active, 1);
    }

    #[test]
    fn new_open_becomes_active() {
        let (app, _dir) = with_two_tabs();
        assert_eq!(app.active, 1);
    }

    #[test]
    fn click_selects_tab() {
        let titles = ["a.fa".to_string(), "t.nwk".to_string()];
        assert_eq!(tab_ranges(&titles), [0..6, 7..14]);

        let (mut app, _dir) = with_two_tabs();
        app.active = 0;
        app.handle_event(&click(9, 1));
        assert_eq!(app.active, 0, "a click below the tab bar selects nothing");
        app.handle_event(&click(6, 0));
        assert_eq!(app.active, 0, "a click on the separator selects nothing");
        app.handle_event(&click(9, 0));
        assert_eq!(app.active, 1);
        app.handle_event(&click(0, 0));
        assert_eq!(app.active, 0);
    }

    #[test]
    fn toggles_log_pane() {
        let (mut app, ..) = setup();
        app.handle_event(&key('L'));
        assert!(app.show_log);
        app.handle_event(&key('L'));
        assert!(!app.show_log);
    }

    #[test]
    fn help_closes_on_any_key() {
        let (mut app, ..) = setup();
        app.handle_event(&key('?'));
        assert!(matches!(app.mode, Mode::Help));
        app.handle_event(&key('x'));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn key_release_is_ignored() {
        let (mut app, ..) = setup();
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_event(&Event::Key(release));
        assert!(!app.quit);
    }
}
