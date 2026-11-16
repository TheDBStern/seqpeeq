//! Interface state and how input events change it. No drawing, no terminal I/O.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::layout::Size;
use seqpeeq_core::{Alignment, Command, Error, Finished, Job, MsaCmd, Progress, Session, Started};

use crate::line::{History, LineEdit};
use crate::msa_view::{MsaView, Shape, parse_goto};
use crate::ui;

pub enum Mode {
    Normal,
    /// Typing one line at the bottom of the screen.
    Prompt(Prompt),
    Help,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub line: LineEdit,
}

/// What a line being typed is for.
pub enum PromptKind {
    /// The `:` command line.
    Command,
    /// Where to move the cursor: a column, or a row and a column.
    Goto,
}

impl PromptKind {
    /// What is shown in front of the text.
    pub fn lead(&self) -> &'static str {
        match self {
            PromptKind::Command => ":",
            PromptKind::Goto => "go to: ",
        }
    }
}

/// Work for the event loop to hand to a worker thread.
pub enum Request {
    /// The slow part of a command.
    Run(Job),
}

/// Work that a worker thread has finished.
pub enum Done {
    Command(Finished),
}

/// The command that is running in the background.
pub struct Busy {
    /// What it is doing, such as `opening a.fa`.
    pub label: String,
    pub progress: Arc<Progress>,
    /// Whether the visible tab should move to the current file when it ends.
    follow: bool,
    /// Whether it came from the queue rather than from the user just now.
    queued: bool,
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
    /// The command running in the background, if any. Until it ends, every
    /// other command is refused.
    pub busy: Option<Busy>,
    /// Work waiting for the event loop to hand to a thread.
    pub outbox: Vec<Request>,
    /// Commands waiting for the session to be free, oldest first.
    pub queue: VecDeque<Command>,
    /// The size of the terminal.
    pub screen: Size,
    /// Where the user is in each open alignment, by [`Dataset::id`].
    ///
    /// [`Dataset::id`]: seqpeeq_core::Dataset::id
    pub views: HashMap<u64, MsaView>,
    /// Lines entered on the command line.
    history: History,
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
            busy: None,
            outbox: Vec::new(),
            queue: VecDeque::new(),
            screen: Size::new(80, 24),
            views: HashMap::new(),
            history: History::default(),
        }
    }

    /// Add a command to run when the session is free.
    pub fn enqueue(&mut self, cmd: Command) {
        self.queue.push_back(cmd);
    }

    /// Called once per turn of the event loop: start queued commands while
    /// nothing is running.
    pub fn pump(&mut self) {
        self.sync_views();
        while self.busy.is_none() {
            let Some(cmd) = self.queue.pop_front() else {
                break;
            };
            let ran = self.run_command(cmd);
            if let Some(busy) = &mut self.busy {
                busy.queued = true;
            }
            self.report(ran, true);
        }
    }

    /// Take in work a thread has finished.
    pub fn handle_done(&mut self, done: Done) {
        match done {
            Done::Command(finished) => {
                let Some(busy) = self.busy.take() else {
                    return;
                };
                let ended = self.session.finish(finished);
                if ended.is_ok()
                    && busy.follow
                    && let Some(current) = self.session.current()
                {
                    self.active = current;
                }
                self.report(ended.map(|outcome| Some(outcome.message)), busy.queued);
            }
        }
    }

    pub fn resize(&mut self, size: Size) {
        if size != self.screen {
            self.screen = size;
            self.reveal();
        }
    }

    /// The alignment on the visible tab and where the user is in it.
    pub fn alignment(&self) -> Option<(&Alignment, &MsaView)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset.msa()?, self.views.get(&dataset.id)?))
    }

    /// The size of the alignment on the visible tab.
    fn shape(&self) -> Option<Shape> {
        let (alignment, _) = self.alignment()?;
        Some(Shape {
            rows: alignment.rows(),
            cols: alignment.cols(),
        })
    }

    fn view_mut(&mut self) -> Option<&mut MsaView> {
        let dataset = self.session.datasets().get(self.active)?;
        self.views.get_mut(&dataset.id)
    }

    /// How many rows and columns of residues are on screen.
    fn screen_shape(&self) -> Shape {
        match ui::grid_of(self) {
            Some(layout) => ui::grid::screen_shape(&layout),
            None => Shape { rows: 0, cols: 0 },
        }
    }

    /// Scroll the visible alignment just enough to show its cursor.
    fn reveal(&mut self) {
        let screen = self.screen_shape();
        if let Some(view) = self.view_mut() {
            view.reveal(screen);
        }
    }

    /// Keep one view per open alignment, each inside its alignment: files
    /// are opened and closed, and edits can shrink what a view points into.
    fn sync_views(&mut self) {
        let mut open = Vec::new();
        for dataset in self.session.datasets() {
            if let Some(alignment) = dataset.msa() {
                let view = self.views.entry(dataset.id).or_insert_with(MsaView::new);
                view.clamp(Shape {
                    rows: alignment.rows(),
                    cols: alignment.cols(),
                });
                open.push(dataset.id);
            }
        }
        self.views.retain(|id, _| open.contains(id));
    }

    /// Background work is running, so the screen should be redrawn soon to
    /// show progress.
    pub fn wants_tick(&self) -> bool {
        self.busy.is_some()
    }

    /// Show how a command went. `None` means it is running in the
    /// background. A queued command never replaces an error on screen:
    /// several run in a row at start-up, and a failure among them must
    /// still be there to read when they are done.
    fn report(&mut self, ran: seqpeeq_core::Result<Option<String>>, queued: bool) {
        let text = match ran {
            Ok(Some(message)) => message,
            Ok(None) => match &self.busy {
                Some(busy) => format!("{}…", busy.label),
                None => return,
            },
            Err(Error::Cancelled) => "cancelled".to_string(),
            Err(error) => format!("error: {error}"),
        };
        if !(queued && self.status.starts_with("error:")) {
            self.status = text;
        }
        self.sync_views();
    }

    pub fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => self.handle_paste(text),
            Event::Resize(width, height) => self.resize(Size::new(*width, *height)),
            _ => {}
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Normal if ctrl => {}
            Mode::Normal => match key.code {
                KeyCode::Esc => {
                    if let Some(busy) = &self.busy {
                        busy.progress.cancel();
                        self.status = "cancelling…".to_string();
                    }
                }
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char(':') => self.open_prompt(PromptKind::Command, ""),
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Char('L') => self.show_log = !self.show_log,
                KeyCode::Tab => self.step_tab(1),
                KeyCode::BackTab => self.step_tab(-1),
                _ => self.grid_key(key),
            },
            Mode::Prompt(prompt) => match key.code {
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Backspace if prompt.line.text().is_empty() => self.mode = Mode::Normal,
                KeyCode::Enter => {
                    let Mode::Prompt(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
                    else {
                        unreachable!("the mode was just matched");
                    };
                    self.submit(prompt);
                }
                KeyCode::Up if matches!(prompt.kind, PromptKind::Command) => {
                    if let Some(older) = self.history.older(prompt.line.text()) {
                        prompt.line.set(older);
                    }
                }
                KeyCode::Down if matches!(prompt.kind, PromptKind::Command) => {
                    if let Some(newer) = self.history.newer() {
                        prompt.line.set(newer);
                    }
                }
                _ => {
                    prompt.line.key(key);
                }
            },
            Mode::Help => self.mode = Mode::Normal,
        }
    }

    /// A key that acts on the alignment on the visible tab.
    fn grid_key(&mut self, key: &KeyEvent) {
        let Some(aln) = self.shape() else {
            return;
        };
        if key.code == KeyCode::Char('g') {
            self.open_prompt(PromptKind::Goto, "");
            return;
        }
        let page = self.screen_shape().rows.max(1) as isize;
        let Some(view) = self.view_mut() else {
            return;
        };
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => view.move_by(0, -1, aln),
            KeyCode::Right | KeyCode::Char('l') => view.move_by(0, 1, aln),
            KeyCode::Up | KeyCode::Char('k') => view.move_by(-1, 0, aln),
            KeyCode::Down | KeyCode::Char('j') => view.move_by(1, 0, aln),
            KeyCode::PageUp => view.move_by(-page, 0, aln),
            KeyCode::PageDown => view.move_by(page, 0, aln),
            KeyCode::Home => view.move_to(None, Some(0), aln),
            KeyCode::End => view.move_to(None, Some(usize::MAX), aln),
            KeyCode::Char('c') => view.cycle_colouring(),
            KeyCode::Char('[') => view.widen_names(-1),
            KeyCode::Char(']') => view.widen_names(1),
            _ => return,
        }
        self.reveal();
    }

    /// Move the cursor to what was typed at the go-to prompt.
    fn go_to(&mut self, text: &str) {
        let Some(aln) = self.shape() else {
            return;
        };
        let target = parse_goto(text).and_then(|(row, col)| {
            if let Some(row) = row.filter(|&row| row >= aln.rows) {
                return Err(format!(
                    "row {} is past the last row ({})",
                    row + 1,
                    aln.rows
                ));
            }
            if let Some(col) = col.filter(|&col| col >= aln.cols) {
                return Err(format!(
                    "column {} is past the last column ({})",
                    col + 1,
                    aln.cols
                ));
            }
            Ok((row, col))
        });
        match target {
            Ok((row, col)) => {
                if let Some(view) = self.view_mut() {
                    view.move_to(row, col, aln);
                }
                self.reveal();
            }
            Err(message) => self.status = format!("error: {message}"),
        }
    }

    /// Scroll the alignment under the mouse wheel. The cursor stays put.
    fn wheel(&mut self, mouse: &MouseEvent) {
        const ROWS: isize = 3;
        const COLS: isize = 10;
        // Few terminals report sideways scrolling; many pass on the wheel
        // with Shift or Alt held instead.
        let sideways = mouse
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let (rows, cols) = match mouse.kind {
            MouseEventKind::ScrollUp if sideways => (0, -COLS),
            MouseEventKind::ScrollDown if sideways => (0, COLS),
            MouseEventKind::ScrollUp => (-ROWS, 0),
            MouseEventKind::ScrollDown => (ROWS, 0),
            MouseEventKind::ScrollLeft => (0, -COLS),
            MouseEventKind::ScrollRight => (0, COLS),
            _ => return,
        };
        let (Some(aln), screen) = (self.shape(), self.screen_shape()) else {
            return;
        };
        if let Some(view) = self.view_mut() {
            view.scroll(rows, cols, aln, screen);
        }
    }

    /// Pasted text goes into a line being typed and nowhere else: letters
    /// are commands outside a prompt, so a paste there could do anything.
    fn handle_paste(&mut self, text: &str) {
        let Mode::Prompt(prompt) = &mut self.mode else {
            self.status = "paste ignored: press : first to paste into the command line".to_string();
            return;
        };
        let (first, rest) = match text.find(['\n', '\r']) {
            Some(end) => (&text[..end], &text[end..]),
            None => (text, ""),
        };
        prompt.line.insert(first);
        if !rest.trim().is_empty() {
            self.status = "pasted text was cut at its first line break".to_string();
        }
    }

    /// Start typing a line, with `text` already on it.
    fn open_prompt(&mut self, kind: PromptKind, text: &str) {
        self.history.reset();
        self.mode = Mode::Prompt(Prompt {
            kind,
            line: LineEdit::new(text),
        });
    }

    /// Act on a line that was entered.
    fn submit(&mut self, prompt: Prompt) {
        match prompt.kind {
            PromptKind::Command => {
                self.history.push(prompt.line.text());
                self.run_line(prompt.line.text());
            }
            PromptKind::Goto => self.go_to(prompt.line.text()),
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.wheel(mouse);
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
            self.reveal();
        }
    }

    /// Run a command on behalf of the user looking at the visible tab. Every
    /// change to the data goes through here.
    ///
    /// Returns the outcome's message, or `None` if the command is slow and
    /// is now running in the background ([`App::busy`] says what it is).
    pub(crate) fn run_command(&mut self, cmd: Command) -> seqpeeq_core::Result<Option<String>> {
        if let Some(busy) = &self.busy {
            return Err(Error::Invalid(format!(
                "still busy: {} (Esc cancels)",
                busy.label
            )));
        }
        let chooses_file = matches!(cmd, Command::Open { .. } | Command::Use { .. });
        // Data commands apply to the session's current file. If that is not
        // the tab on screen, say so first, so the log shows which file each
        // command changed.
        if !chooses_file
            && !self.session.datasets().is_empty()
            && self.session.current() != Some(self.active)
        {
            self.session.execute(Command::Use {
                dataset: self.active + 1,
            })?;
        }
        let follow = chooses_file || cmd == Command::Close;
        match self.session.start(cmd)? {
            Started::Done(outcome) => {
                if follow {
                    self.active = self.session.current().unwrap_or(0);
                }
                Ok(Some(outcome.message))
            }
            Started::Running(job) => {
                self.busy = Some(Busy {
                    label: job.label().to_string(),
                    progress: job.progress(),
                    follow,
                    queued: false,
                });
                self.outbox.push(Request::Run(job));
                Ok(None)
            }
        }
    }

    /// Run one line typed on the command line.
    fn run_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        // Leaving is an interface action, not a change to the data: not logged.
        if line == "q" || line == "quit" {
            self.quit = true;
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let ran = Command::parse_line(line)
            .map(|cmd| expand_home(cmd, home.as_deref()))
            .and_then(|cmd| self.run_command(cmd));
        self.report(ran, false);
    }
}

/// Put the home directory in place of a leading `~` in the path a typed
/// command names, so that the log holds a path that means the same thing
/// to whoever replays it.
fn expand_home(mut cmd: Command, home: Option<&Path>) -> Command {
    let path = match &mut cmd {
        Command::Open { path, .. } => path,
        Command::Msa(MsaCmd::Extract { out, .. }) => out,
        _ => return cmd,
    };
    if let (Some(home), Ok(rest)) = (home, path.strip_prefix("~")) {
        *path = if rest.as_os_str().is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        };
    }
    cmd
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

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn paste(text: &str) -> Event {
        Event::Paste(text.to_string())
    }

    /// The text of the prompt being typed into, if one is open.
    fn typed(app: &App) -> Option<&str> {
        match &app.mode {
            Mode::Prompt(prompt) => Some(prompt.line.text()),
            _ => None,
        }
    }

    /// Do, on this thread, the work the loop would hand to worker threads.
    fn settle(app: &mut App) {
        loop {
            app.pump();
            let Some(request) = app.outbox.pop() else {
                break;
            };
            match request {
                Request::Run(job) => app.handle_done(Done::Command(job.run())),
            }
        }
    }

    fn enter(app: &mut App) {
        app.handle_event(&press(KeyCode::Enter));
        settle(app);
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
        app.handle_event(&ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn q_inside_command_buffer_is_text() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":open q");
        assert!(!app.quit);
        assert_eq!(typed(&app), Some("open q"));
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
        assert_eq!(typed(&app), Some(""));
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

    const TRIO: &str = ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n";

    /// An app with two ten-column alignments open; the second is visible.
    fn with_two_alignments() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Session::new(None));
        for name in ["a.fa", "b.fa"] {
            let path = dir.path().join(name);
            fs::write(&path, TRIO).unwrap();
            type_str(&mut app, &format!(":open {}", path.display()));
            enter(&mut app);
        }
        (app, dir)
    }

    fn cols(app: &App, dataset: usize) -> usize {
        app.session.datasets()[dataset].msa().unwrap().cols()
    }

    #[test]
    fn a_typed_command_applies_to_the_visible_tab() {
        let (mut app, _dir) = with_two_alignments();
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!((app.active, app.session.current()), (0, Some(1)));

        type_str(&mut app, ":msa delete-cols --cols 1");
        enter(&mut app);
        let history = app.session.history();
        assert_eq!(
            history[history.len() - 2..],
            ["use 1", "msa delete-cols --cols 1"]
        );
        assert_eq!((cols(&app, 0), cols(&app, 1)), (9, 10));

        // The file is now current, so no second `use` is needed.
        let before = app.session.history().len();
        type_str(&mut app, ":msa delete-cols --cols 1");
        enter(&mut app);
        assert_eq!(app.session.history().len(), before + 1);
        assert_eq!((cols(&app, 0), cols(&app, 1)), (8, 10));
    }

    #[test]
    fn typed_use_moves_the_visible_tab() {
        let (mut app, _dir) = with_two_alignments();
        assert_eq!(app.active, 1);
        type_str(&mut app, ":use 1");
        enter(&mut app);
        assert_eq!((app.active, app.status.as_str()), (0, "using 1: a.fa"));
        type_str(&mut app, ":use 9");
        enter(&mut app);
        assert_eq!(app.status, "error: there is no file 9 (2 are open)");
        assert_eq!(app.active, 0);
    }

    #[test]
    fn opening_a_file_does_not_log_a_use_first() {
        let (mut app, dir) = with_two_alignments();
        app.handle_event(&press(KeyCode::Tab));
        let path = dir.path().join("a.fa");
        type_str(&mut app, &format!(":open {}", path.display()));
        enter(&mut app);
        assert_eq!(app.session.history().len(), 3);
        assert_eq!(app.active, 2);
    }

    #[test]
    fn a_paste_outside_a_prompt_does_nothing() {
        let (mut app, _dir) = with_two_alignments();
        let before = app.session.history().len();
        app.handle_event(&paste("dd\nq\n:msa delete-rows --rows 1\n"));
        assert!(!app.quit && matches!(app.mode, Mode::Normal));
        assert_eq!(app.session.history().len(), before);
        assert_eq!(
            app.status,
            "paste ignored: press : first to paste into the command line"
        );
        app.mode = Mode::Help;
        app.handle_event(&paste("q"));
        assert!(!app.quit && matches!(app.mode, Mode::Help));
    }

    #[test]
    fn a_paste_into_the_command_line_stops_at_the_first_line_break() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":open ");
        app.handle_event(&paste("/data/x.fa\nq\n"));
        assert_eq!(typed(&app), Some("open /data/x.fa"));
        assert_eq!(app.status, "pasted text was cut at its first line break");
        assert!(app.session.history().is_empty(), "nothing ran");

        // A line break at the very end is not worth a warning.
        let (mut app, ..) = setup();
        type_str(&mut app, ":");
        app.handle_event(&paste("undo\r\n"));
        assert_eq!((typed(&app), app.status.as_str()), (Some("undo"), ""));
    }

    #[test]
    fn the_command_line_is_edited_in_place_and_remembers_lines() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":opn x");
        for _ in 0..3 {
            app.handle_event(&press(KeyCode::Left));
        }
        type_str(&mut app, "e");
        assert_eq!(typed(&app), Some("open x"));
        enter(&mut app);
        assert!(app.status.starts_with("error: "), "{}", app.status);
        type_str(&mut app, ":");
        app.handle_event(&press(KeyCode::Up));
        assert_eq!(typed(&app), Some("open x"));
        app.handle_event(&press(KeyCode::Down));
        assert_eq!(typed(&app), Some(""));
        // Leaving part-way through the history does not carry over.
        app.handle_event(&press(KeyCode::Up));
        app.handle_event(&press(KeyCode::Esc));
        type_str(&mut app, ":");
        app.handle_event(&ctrl('a'));
        app.handle_event(&press(KeyCode::Up));
        assert_eq!(typed(&app), Some("open x"));
    }

    #[test]
    fn an_empty_command_line_is_not_an_error() {
        let (mut app, ..) = setup();
        app.status = "kept".to_string();
        type_str(&mut app, ":  ");
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status, "kept");
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        use std::path::Path;
        let home = Path::new("/home/u");
        let open = |path: &str| Command::Open {
            path: path.into(),
            kind: None,
        };
        assert_eq!(
            expand_home(open("~/x.fa"), Some(home)),
            open("/home/u/x.fa")
        );
        assert_eq!(expand_home(open("~"), Some(home)), open("/home/u"));
        assert_eq!(
            expand_home(open("~user/x.fa"), Some(home)),
            open("~user/x.fa")
        );
        assert_eq!(expand_home(open("a~/x.fa"), Some(home)), open("a~/x.fa"));
        assert_eq!(expand_home(open("~/x.fa"), None), open("~/x.fa"));
        let extract = Command::parse_line("msa extract --out ~/o.fa").unwrap();
        assert_eq!(
            expand_home(extract, Some(home)).to_line().unwrap(),
            "msa extract --out /home/u/o.fa"
        );
    }

    #[test]
    fn an_open_runs_in_the_background() {
        let (mut app, _dir, fa, _) = setup();
        type_str(&mut app, &format!(":open {}", fa.display()));
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(app.status, "opening a.fa…");
        assert!(app.wants_tick() && app.session.datasets().is_empty());
        assert_eq!(app.outbox.len(), 1);
        settle(&mut app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
        assert!(!app.wants_tick() && app.busy.is_none());
        assert_eq!((app.session.datasets().len(), app.active), (1, 0));
    }

    #[test]
    fn commands_are_refused_while_a_job_runs() {
        let (mut app, dir) = with_two_alignments();
        type_str(
            &mut app,
            &format!(":open {}", dir.path().join("a.fa").display()),
        );
        app.handle_event(&press(KeyCode::Enter));
        let before = app.session.history().len();
        type_str(&mut app, ":msa delete-cols --cols 1");
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(app.status, "error: still busy: opening a.fa (Esc cancels)");
        assert_eq!(app.session.history().len(), before);
        assert_eq!(
            cols(&app, 1),
            10,
            "the refused command was not kept for later"
        );
        settle(&mut app);
        assert_eq!(cols(&app, 1), 10);
        assert_eq!(app.session.datasets().len(), 3);
        assert_eq!(app.active, 2, "the file that was opened is shown");
    }

    #[test]
    fn escape_cancels_the_running_job() {
        let (mut app, _dir, fa, _) = setup();
        type_str(&mut app, &format!(":open {}", fa.display()));
        app.handle_event(&press(KeyCode::Enter));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(app.status, "cancelling…");
        settle(&mut app);
        assert_eq!(app.status, "cancelled");
        assert!(app.session.datasets().is_empty() && app.busy.is_none());
    }

    #[test]
    fn queued_opens_run_one_after_another_and_an_error_stays_visible() {
        let (mut app, dir, fa, nwk) = setup();
        let bad = dir.path().join("bad.fa");
        fs::write(&bad, "this is not fasta").unwrap();
        for path in [&fa, &bad, &nwk] {
            app.enqueue(Command::Open {
                path: path.clone(),
                kind: None,
            });
        }
        settle(&mut app);
        assert_eq!(app.session.datasets().len(), 2);
        assert!(
            app.status.starts_with("error: ") && app.status.contains("bad.fa"),
            "{}",
            app.status
        );
        assert!(app.queue.is_empty());
        // What the user does next is reported as usual.
        type_str(&mut app, ":use 1");
        enter(&mut app);
        assert_eq!(app.status, "using 1: a.fa");
    }

    #[test]
    fn a_tab_chosen_during_an_extract_stays_chosen() {
        let (mut app, dir) = with_two_alignments();
        type_str(
            &mut app,
            &format!(":msa extract --out {}", dir.path().join("o.fa").display()),
        );
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(app.status, "writing o.fa…");
        app.handle_event(&press(KeyCode::Tab));
        settle(&mut app);
        assert!(app.status.starts_with("wrote "), "{}", app.status);
        assert_eq!(app.active, 0);
    }

    use crate::msa_view::{Cell, Colouring, MsaView};
    use crate::ui::grid::{GridLayout, cursor_position};
    use ratatui::layout::Size;

    /// An app showing the 5 x 60 fixture on a 70 x 9 screen.
    fn with_grid() -> App {
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(70, 9));
        app.enqueue(Command::Open {
            path: "tests/data/grid.fa".into(),
            kind: None,
        });
        settle(&mut app);
        app
    }

    /// The view of the alignment on the visible tab.
    fn view(app: &App) -> &MsaView {
        app.alignment().expect("an alignment is shown").1
    }

    fn active_layout(app: &App) -> GridLayout {
        ui::grid_of(app).expect("an alignment is shown")
    }

    fn wheel(kind: MouseEventKind, modifiers: KeyModifiers) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 30,
            row: 3,
            modifiers,
        })
    }

    #[test]
    fn keys_move_the_cursor_and_the_screen_follows() {
        let mut app = with_grid();
        for c in "lllj".chars() {
            app.handle_event(&key(c));
        }
        assert_eq!(view(&app).cursor, Cell { row: 1, col: 3 });
        app.handle_event(&key('h'));
        app.handle_event(&key('k'));
        app.handle_event(&press(KeyCode::Right));
        app.handle_event(&press(KeyCode::Down));
        assert_eq!(view(&app).cursor, Cell { row: 1, col: 3 });
        app.handle_event(&press(KeyCode::End));
        assert_eq!(view(&app).cursor.col, 59);
        assert_eq!(
            view(&app).left,
            17,
            "43 columns fit; the last one is on screen"
        );
        app.handle_event(&press(KeyCode::Home));
        assert_eq!((view(&app).cursor.col, view(&app).left), (0, 0));
        app.handle_event(&press(KeyCode::PageDown));
        assert_eq!(view(&app).cursor.row, 4, "stops at the last row");
        app.handle_event(&press(KeyCode::PageUp));
        assert_eq!(view(&app).cursor.row, 0);
    }

    #[test]
    fn go_to_moves_or_explains() {
        let mut app = with_grid();
        type_str(&mut app, "g3,50");
        enter(&mut app);
        assert_eq!(view(&app).cursor, Cell { row: 2, col: 49 });
        assert_eq!(view(&app).left, 7);
        for (typed, status) in [
            ("g99,1", "error: row 99 is past the last row (5)"),
            ("g999", "error: column 999 is past the last column (60)"),
            (
                "gx",
                "error: go to: give a column (27), a row and column (12,27) or a row (12,)",
            ),
        ] {
            type_str(&mut app, typed);
            enter(&mut app);
            assert_eq!(app.status, status);
            assert_eq!(view(&app).cursor, Cell { row: 2, col: 49 });
        }
        assert_eq!(app.session.history().len(), 1, "moving is not logged");
    }

    #[test]
    fn the_wheel_scrolls_and_view_keys_change_only_the_view() {
        let mut app = with_grid();
        app.handle_event(&wheel(MouseEventKind::ScrollRight, KeyModifiers::NONE));
        assert_eq!((view(&app).left, view(&app).cursor.col), (10, 0));
        app.handle_event(&wheel(MouseEventKind::ScrollUp, KeyModifiers::SHIFT));
        assert_eq!(view(&app).left, 0);
        app.handle_event(&wheel(MouseEventKind::ScrollDown, KeyModifiers::NONE));
        assert_eq!(
            view(&app).top,
            0,
            "five rows fit, so there is nothing to scroll"
        );
        app.handle_event(&key(']'));
        app.handle_event(&key('c'));
        assert_eq!(
            (view(&app).name_width, view(&app).colouring),
            (28, Colouring::Residue)
        );
        assert_eq!(app.session.history().len(), 1);
    }

    #[test]
    fn each_file_keeps_its_own_place_and_closing_forgets_it() {
        let (mut app, _dir) = with_two_alignments();
        app.handle_event(&key('l'));
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(view(&app).cursor, Cell::default());
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(view(&app).cursor, Cell { row: 0, col: 1 });
        type_str(&mut app, ":close");
        enter(&mut app);
        assert_eq!(app.views.len(), 1);
        assert_eq!(app.active, 0);
    }

    #[test]
    fn shrinking_the_terminal_keeps_the_cursor_on_screen() {
        let mut app = with_grid();
        app.handle_event(&press(KeyCode::End));
        app.handle_event(&Event::Resize(40, 6));
        let l = active_layout(&app);
        assert!(cursor_position(&l, view(&app)).is_some());
    }

    #[test]
    fn grid_keys_are_harmless_without_an_alignment() {
        let (mut app, ..) = setup();
        for c in "hjklgc[]".chars() {
            app.handle_event(&key(c));
        }
        app.handle_event(&wheel(MouseEventKind::ScrollDown, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal) && !app.quit);
    }
}
