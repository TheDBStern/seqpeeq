//! `p` on a tree tab: pick the open alignment the tree links to.

use super::*;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

/// The open alignments to link a tree to, best match first.
#[derive(Debug, Clone)]
pub struct LinkList {
    /// The id of the tree.
    pub tree: u64,
    /// The tree's file name, for the title.
    pub name: String,
    pub rows: Vec<LinkRow>,
    /// The row picked.
    pub cursor: usize,
    /// The first row shown.
    pub top: usize,
}

#[derive(Debug, Clone)]
pub enum LinkRow {
    Link {
        name: String,
        path: PathBuf,
        shared: usize,
        tips: usize,
        linked: bool,
    },
    Unlink {
        name: String,
    },
}

impl LinkRow {
    pub fn text(&self) -> String {
        match self {
            LinkRow::Link {
                name,
                shared,
                tips,
                linked,
                ..
            } => {
                let mut text = format!("{name}   {shared} of {tips} tips");
                if *linked {
                    text.push_str("  (linked)");
                }
                text
            }
            LinkRow::Unlink { name } => format!("unlink from {name}"),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

impl App {
    /// `p`: list the open alignments to link the tree to.
    pub(super) fn open_links(&mut self) {
        let Some((dataset, _, _)) = self.tree() else {
            return;
        };
        let (tree, name) = (dataset.id, file_name(&dataset.path));
        let linked = self.session.link_of(tree);
        let mut rows: Vec<LinkRow> = self
            .session
            .datasets()
            .iter()
            .filter_map(|d| {
                let (shared, tips) = self.session.shared_tips(tree, d.id)?;
                Some(LinkRow::Link {
                    name: file_name(&d.path),
                    path: d.path.clone(),
                    shared,
                    tips,
                    linked: linked == Some(d.id),
                })
            })
            .collect();
        if rows.is_empty() {
            self.status = "error: no alignment is open; :open FILE first".to_string();
            return;
        }
        let rank = |row: &LinkRow| match row {
            LinkRow::Link { name, shared, .. } => (std::cmp::Reverse(*shared), name.clone()),
            LinkRow::Unlink { .. } => (std::cmp::Reverse(0), String::new()),
        };
        rows.sort_by_key(rank);
        let unlink = rows.iter().find_map(|row| match row {
            LinkRow::Link {
                name, linked: true, ..
            } => Some(name.clone()),
            _ => None,
        });
        if let Some(name) = unlink {
            rows.push(LinkRow::Unlink { name });
        }
        self.mode = Mode::Links(Box::new(LinkList {
            tree,
            name,
            rows,
            cursor: 0,
            top: 0,
        }));
    }

    /// A key while the link list is open.
    pub(super) fn links_key(&mut self, key: &KeyEvent) {
        let visible = match &self.mode {
            Mode::Links(list) => ui::link::link_box(self, list)
                .height
                .saturating_sub(2)
                .max(1),
            _ => return,
        } as usize;
        let Mode::Links(list) = &mut self.mode else {
            return;
        };
        let last = list.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Enter => {
                let cmd = match list.rows.get(list.cursor) {
                    Some(LinkRow::Link { path, .. }) => Some(TreeCmd::Link { file: path.clone() }),
                    Some(LinkRow::Unlink { .. }) => Some(TreeCmd::Unlink),
                    None => None,
                };
                let tree = list.tree;
                self.mode = Mode::Normal;
                // A file that finished opening meanwhile is shown instead.
                let still = self.tree().is_some_and(|(d, _, _)| d.id == tree);
                if let Some(cmd) = cmd.filter(|_| still) {
                    self.act(Command::Tree(cmd));
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => list.cursor = (list.cursor + 1).min(last),
            KeyCode::Home => list.cursor = 0,
            KeyCode::End => list.cursor = last,
            _ => {}
        }
        if list.cursor < list.top {
            list.top = list.cursor;
        } else if list.cursor >= list.top + visible {
            list.top = list.cursor + 1 - visible;
        }
    }

    /// The mouse while the link list is open: the wheel moves through it,
    /// a click on a row picks it, and a click outside closes the list.
    pub(super) fn links_mouse(&mut self, mouse: &MouseEvent) {
        let Mode::Links(list) = &self.mode else {
            return;
        };
        let code = match mouse.kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            MouseEventKind::Down(MouseButton::Left) => {
                match ui::link::link_hit(self, list, mouse.column, mouse.row) {
                    ui::link::LinkHit::Row(i) => {
                        if let Mode::Links(list) = &mut self.mode {
                            list.cursor = i;
                        }
                        KeyCode::Enter
                    }
                    ui::link::LinkHit::Outside => KeyCode::Esc,
                    ui::link::LinkHit::Inside => return,
                }
            }
            _ => return,
        };
        self.links_key(&KeyEvent::new(code, KeyModifiers::NONE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::settle;
    use seqpeeq_core::Session;
    use std::fs;
    use tempfile::TempDir;

    fn press(app: &mut App, code: KeyCode) {
        app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn click(app: &mut App, column: u16, row: u16) {
        app.handle_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }

    fn open(app: &mut App, path: PathBuf) {
        app.enqueue(Command::Open { path, kind: None });
        settle(app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
    }

    /// `part.fa` (A B), then `full.fa` (A B C D), then the tree
    /// `((A,B),(C,D));`, which links to `full.fa` by itself.
    fn with_pair() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        fs::write(path("part.fa"), ">A\nAC\n>B\nAG\n").unwrap();
        fs::write(path("full.fa"), ">A\nAC\n>B\nAG\n>C\nTT\n>D\nGG\n").unwrap();
        fs::write(path("t.nwk"), "((A,B),(C,D));").unwrap();
        let mut app = App::new(Session::new(None));
        app.resize(ratatui::layout::Size::new(80, 12));
        for name in ["part.fa", "full.fa", "t.nwk"] {
            open(&mut app, path(name));
        }
        (app, dir)
    }

    fn texts(app: &App) -> Vec<String> {
        match &app.mode {
            Mode::Links(list) => list.rows.iter().map(LinkRow::text).collect(),
            _ => panic!("the link list is open"),
        }
    }

    fn tree_id(app: &App) -> u64 {
        app.session.datasets()[2].id
    }

    fn path_of(app: &App, index: usize) -> PathBuf {
        app.session.datasets()[index].path.clone()
    }

    #[test]
    fn p_lists_open_alignments_best_match_first() {
        let (mut app, _dir) = with_pair();
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(
            texts(&app),
            [
                "full.fa   4 of 4 tips  (linked)",
                "part.fa   2 of 4 tips",
                "unlink from full.fa",
            ]
        );
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn picking_a_row_links_it_and_is_logged() {
        let (mut app, _dir) = with_pair();
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.session.link_of(tree_id(&app)),
            Some(app.session.datasets()[0].id)
        );
        assert_eq!(
            Command::parse_line(app.session.history().last().unwrap()).unwrap(),
            Command::Tree(TreeCmd::Link {
                file: path_of(&app, 0)
            })
        );
    }

    #[test]
    fn picking_unlink_unlinks() {
        let (mut app, _dir) = with_pair();
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(app.session.link_of(tree_id(&app)), None);
    }

    #[test]
    fn p_with_no_alignment_says_so() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("t.nwk"), "((A,B),(C,D));").unwrap();
        let mut app = App::new(Session::new(None));
        open(&mut app, dir.path().join("t.nwk"));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.status, "error: no alignment is open; :open FILE first");
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn the_link_list_takes_the_mouse() {
        let (mut app, _dir) = with_pair();
        press(&mut app, KeyCode::Char('p'));
        let Mode::Links(list) = &app.mode else {
            panic!("the link list is open");
        };
        let area = crate::ui::link::link_box(&app, list);
        // The second row, inside the border.
        click(&mut app, area.x + 2, area.y + 2);
        settle(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.session.link_of(tree_id(&app)),
            Some(app.session.datasets()[0].id)
        );

        press(&mut app, KeyCode::Char('p'));
        click(&mut app, 0, 0);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn link_list() {
        let (mut app, _dir) = with_pair();
        press(&mut app, KeyCode::Char('p'));
        // The status names a temporary folder.
        app.status.clear();
        let screen = crate::ui::tests::rows(&crate::ui::tests::render(&app, 80, 12));
        insta::assert_snapshot!(screen.join("\n"));
    }
}
