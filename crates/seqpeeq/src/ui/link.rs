//! `p` on a tree tab: the box of open alignments to link the tree to.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Clear};

use crate::app::App;
use crate::app::link::LinkList;
use crate::palette;

/// What is at a point of the screen while the link list is open.
pub enum LinkHit {
    /// The row with this index.
    Row(usize),
    /// Inside the box, but on no row.
    Inside,
    Outside,
}

/// Where the box goes: centred in the body, at most 60 columns wide and
/// as tall as its rows.
pub fn link_box(app: &App, list: &LinkList) -> Rect {
    let screen = Rect::new(0, 0, app.screen.width, app.screen.height);
    let body = crate::ui::regions(app, screen).body;
    let width = body.width.min(60);
    let height = (list.rows.len() as u16).saturating_add(2).min(body.height);
    Rect::new(
        body.x + (body.width - width) / 2,
        body.y + (body.height - height) / 2,
        width,
        height,
    )
}

/// What is at `column`, `row`, as `draw_links` lays the box out.
pub fn link_hit(app: &App, list: &LinkList, column: u16, row: u16) -> LinkHit {
    let area = link_box(app, list);
    let at = Position::new(column, row);
    if !area.contains(at) {
        return LinkHit::Outside;
    }
    let inner = Block::bordered().inner(area);
    if !inner.contains(at) {
        return LinkHit::Inside;
    }
    let i = list.top + (row - inner.y) as usize;
    if i < list.rows.len() {
        LinkHit::Row(i)
    } else {
        LinkHit::Inside
    }
}

pub fn draw_links(frame: &mut Frame, app: &App, list: &LinkList) {
    let area = link_box(app, list);
    let block = Block::bordered().title(format!("link {} to", list.name));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let buf = frame.buffer_mut();
    for (i, row) in list.rows.iter().enumerate().skip(list.top) {
        let y = inner.y + (i - list.top) as u16;
        if y >= inner.bottom() {
            break;
        }
        let style = if i == list.cursor {
            palette::SELECTED
        } else {
            Style::new()
        };
        let text = format!("{:<width$}", row.text(), width = inner.width as usize);
        buf.set_stringn(inner.x, y, text, inner.width as usize, style);
    }
}
