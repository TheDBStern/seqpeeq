//! One line of text being typed, and the lines typed before it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A line of text with a cursor in it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineEdit {
    text: String,
    /// A byte index into `text`, always on a character boundary.
    cursor: usize,
}

impl LineEdit {
    /// A line holding `text`, with the cursor at its end.
    pub fn new(text: &str) -> LineEdit {
        LineEdit {
            text: text.to_string(),
            cursor: text.len(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the cursor is, as a byte index into [`LineEdit::text`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put `text` in at the cursor. Control characters are left out, so
    /// pasted text cannot smuggle in escape codes or line breaks.
    pub fn insert(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.text.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    /// Replace the whole line, leaving the cursor at its end.
    pub fn set(&mut self, text: &str) {
        *self = LineEdit::new(text);
    }

    /// Apply an editing key. False if the key is not one.
    pub fn key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let before = &self.text[..self.cursor];
                let word = before.trim_end_matches(' ');
                let start = word.rfind(' ').map_or(0, |space| space + 1);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl && !alt => self.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Left => self.cursor = self.before(),
            KeyCode::Right => self.cursor = self.after(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                let start = self.before();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete => {
                let end = self.after();
                self.text.drain(self.cursor..end);
            }
            _ => return false,
        }
        true
    }

    /// The start of the character before the cursor.
    fn before(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    /// The end of the character under the cursor.
    fn after(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.text.len(), |c| self.cursor + c.len_utf8())
    }
}

/// Lines entered earlier, walked with the Up and Down keys.
#[derive(Debug, Default)]
pub struct History {
    lines: Vec<String>,
    /// The line being shown while walking; `None` when at the draft.
    at: Option<usize>,
    /// What was being typed when the walk began.
    draft: String,
}

impl History {
    /// Remember a line. Blank lines and a repeat of the last line are not
    /// kept.
    pub fn push(&mut self, line: &str) {
        self.at = None;
        if line.trim().is_empty() || self.lines.last().is_some_and(|last| last == line) {
            return;
        }
        self.lines.push(line.to_string());
    }

    /// Start the next walk from the newest line.
    pub fn reset(&mut self) {
        self.at = None;
    }

    /// The line before the one shown, if there is one. `current` is what is
    /// on the line now; it is kept as the draft when the walk begins.
    pub fn older(&mut self, current: &str) -> Option<&str> {
        let to = match self.at {
            None => {
                self.draft = current.to_string();
                self.lines.len().checked_sub(1)?
            }
            Some(at) => at.checked_sub(1)?,
        };
        self.at = Some(to);
        Some(&self.lines[to])
    }

    /// The line after the one shown; past the newest, the draft.
    pub fn newer(&mut self) -> Option<&str> {
        let at = self.at?;
        if at + 1 < self.lines.len() {
            self.at = Some(at + 1);
            Some(&self.lines[at + 1])
        } else {
            self.at = None;
            Some(&self.draft)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn edits_happen_at_the_cursor() {
        let mut line = LineEdit::new("opn");
        line.key(&key(KeyCode::Left));
        line.insert("e");
        assert_eq!((line.text(), line.cursor()), ("open", 3));
        line.key(&key(KeyCode::Home));
        line.key(&key(KeyCode::Delete));
        assert_eq!((line.text(), line.cursor()), ("pen", 0));
        line.key(&key(KeyCode::Backspace));
        assert_eq!(line.text(), "pen", "nothing before the start to delete");
        line.key(&key(KeyCode::End));
        line.key(&key(KeyCode::Backspace));
        assert_eq!((line.text(), line.cursor()), ("pe", 2));
        assert!(!line.key(&key(KeyCode::Up)), "not an editing key");
        assert!(line.key(&key(KeyCode::Char('n'))));
        assert_eq!(line.text(), "pen");
    }

    #[test]
    fn a_character_of_several_bytes_is_one_step() {
        let mut line = LineEdit::new("aé配");
        line.key(&key(KeyCode::Left));
        assert_eq!(line.cursor(), 3);
        line.key(&key(KeyCode::Backspace));
        assert_eq!((line.text(), line.cursor()), ("a配", 1));
        line.key(&key(KeyCode::Right));
        line.key(&key(KeyCode::Right));
        assert_eq!(line.cursor(), 4);
    }

    #[test]
    fn ctrl_w_and_ctrl_u() {
        let mut line = LineEdit::new("msa fill --with N");
        line.key(&ctrl('w'));
        assert_eq!(line.text(), "msa fill --with ");
        line.key(&ctrl('w'));
        assert_eq!(line.text(), "msa fill ");
        line.key(&ctrl('a'));
        assert_eq!(line.cursor(), 0);
        line.key(&ctrl('e'));
        line.key(&ctrl('u'));
        assert_eq!((line.text(), line.cursor()), ("", 0));
    }

    #[test]
    fn inserted_text_loses_its_control_characters() {
        let mut line = LineEdit::new("");
        line.insert("a\tb\u{1b}[31mc");
        assert_eq!(line.text(), "ab[31mc");
    }

    #[test]
    fn set_replaces_everything() {
        let mut line = LineEdit::new("abc");
        line.key(&key(KeyCode::Home));
        line.set("xy");
        assert_eq!((line.text(), line.cursor()), ("xy", 2));
    }

    #[test]
    fn history_walks_back_and_returns_to_the_draft() {
        let mut history = History::default();
        for line in ["open a.fa", "  ", "undo", "undo"] {
            history.push(line);
        }
        assert_eq!(history.older("dra"), Some("undo"));
        assert_eq!(history.older("ignored"), Some("open a.fa"));
        assert_eq!(history.older("ignored"), None, "nothing older");
        assert_eq!(history.newer(), Some("undo"));
        assert_eq!(history.newer(), Some("dra"));
        assert_eq!(history.newer(), None);
        // A walk that was left part-way starts again from the newest line.
        history.older("x");
        history.older("x");
        history.reset();
        assert_eq!(history.older("y"), Some("undo"));
    }
}
