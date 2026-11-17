//! Drawing a fitted tree, and finding what a click lands on.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use seqpeeq_core::{NodeId, TreeDoc};
use unicode_width::UnicodeWidthStr;

use super::grid::fit;
use crate::palette;
use crate::tree_view::{Row, TreeView};

/// Where the parts of the tree view go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLayout {
    pub header: Rect,
    pub rows: Rect,
    /// The scale bar's row; empty when drawn with equal lengths or when
    /// there is no room.
    pub scale: Rect,
    /// Columns for branches; names get the rest.
    pub branch_width: u16,
}

/// Whether the shown clade is drawn to scale: lengths are wanted and some
/// tip in it lies beyond it.
fn to_scale(doc: &TreeDoc, view: &TreeView) -> bool {
    let facts = doc.facts();
    !view.equal && facts.reach(view.shown) > facts.depth(view.shown)
}

pub fn tree_layout(area: Rect, doc: &TreeDoc, view: &TreeView) -> TreeLayout {
    let header = Rect {
        height: area.height.min(1),
        ..area
    };
    let rest = area.height - header.height;
    let scale_height = u16::from(to_scale(doc, view) && rest >= 2);
    let rows = Rect::new(area.x, header.bottom(), area.width, rest - scale_height);
    let scale = Rect::new(area.x, rows.bottom(), area.width, scale_height);
    let end = (view.top + rows.height as usize).min(view.rows.len());
    let widest = view.rows[view.top.min(end)..end]
        .iter()
        .map(|row| row_text(doc, view, row).width())
        .max()
        .unwrap_or(0);
    let width = area.width as usize;
    let branch_width = (width / 2).max(width.saturating_sub(widest + 1));
    TreeLayout {
        header,
        rows,
        scale,
        branch_width: branch_width as u16,
    }
}

/// An open node as drawn: its branch runs from its parent's column `from`
/// to its junction at column `x`; its vertical line spans the rows of its
/// first and last child.
#[derive(Clone, Copy, Debug)]
struct Open {
    from: usize,
    x: usize,
    first: usize,
    last: usize,
}

/// Columns for every row and open node, relative to the rows' area.
struct Geometry {
    /// Per row: its parent's column and the column its branch ends at.
    rows: Vec<(usize, usize)>,
    open: HashMap<NodeId, Open>,
    /// Columns per unit of length; `None` when drawn with equal lengths.
    scale: Option<f64>,
    /// The rows of the selected clade, when on screen.
    selected: Option<(usize, usize)>,
}

fn geometry(doc: &TreeDoc, view: &TreeView, branch_width: usize) -> Geometry {
    let (tree, facts) = (doc.tree(), doc.facts());
    let shown = view.shown;
    let length = |n: NodeId| f64::from(facts.depth(n) - facts.depth(shown));
    let steps = |n: NodeId| f64::from(facts.steps(n).saturating_sub(facts.steps(shown)));
    let farthest = |dist: &dyn Fn(NodeId) -> f64| {
        view.rows
            .iter()
            .map(|row| dist(row.node))
            .fold(0.0, f64::max)
    };
    let mut equal = view.equal;
    let mut far = if equal { 0.0 } else { farthest(&length) };
    if far <= 0.0 {
        equal = true;
        far = farthest(&steps);
    }
    let span = branch_width.saturating_sub(2) as f64;
    let per = if far > 0.0 { span / far } else { 0.0 };
    let place = |n: NodeId, from: usize| {
        let dist = if equal { steps(n) } else { length(n) };
        (1 + (dist * per).round().max(0.0) as usize).max(from + 1)
    };
    let mut g = Geometry {
        rows: Vec::with_capacity(view.rows.len()),
        open: HashMap::new(),
        scale: (!equal).then_some(per),
        selected: None,
    };
    if !view.is_open(shown) {
        g.rows.push((0, 1));
        if view.select == Some(shown) {
            g.selected = Some((0, 0));
        }
        return g;
    }
    // Each frame: a node, its column, its parent's column, its children
    // still to visit, the rows of its first and last child, and the row
    // count when it was entered.
    let mut stack = vec![(shown, 1, 0, tree.children(shown), None, 0, 0)];
    loop {
        let Some(frame) = stack.last_mut() else {
            break;
        };
        let (n, x) = (frame.0, frame.1);
        match frame.3.next() {
            Some(c) => {
                let y = if view.is_open(c) {
                    view.junction.get(&c).copied().unwrap_or(0)
                } else {
                    g.rows.len()
                };
                frame.4.get_or_insert(y);
                frame.5 = y;
                if view.is_open(c) {
                    let rows = g.rows.len();
                    stack.push((c, place(c, x), x, tree.children(c), None, 0, rows));
                } else {
                    if view.select == Some(c) {
                        g.selected = Some((y, y));
                    }
                    g.rows.push((x, place(c, x)));
                }
            }
            None => {
                let (_, x, from, _, first, last, entered) = stack.pop().expect("a frame");
                g.open.insert(
                    n,
                    Open {
                        from,
                        x,
                        first: first.unwrap_or(last),
                        last,
                    },
                );
                if view.select == Some(n) && g.rows.len() > entered {
                    g.selected = Some((entered, g.rows.len() - 1));
                }
            }
        }
    }
    g
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

fn line_char(bits: u8) -> Option<char> {
    Some(match bits {
        0 => return None,
        b if b == UP | DOWN | LEFT | RIGHT => '┼',
        b if b == UP | DOWN | RIGHT => '├',
        b if b == UP | DOWN | LEFT => '┤',
        b if b == DOWN | LEFT | RIGHT => '┬',
        b if b == UP | LEFT | RIGHT => '┴',
        b if b == DOWN | RIGHT => '┌',
        b if b == UP | RIGHT => '└',
        b if b == DOWN | LEFT => '┐',
        b if b == UP | LEFT => '┘',
        b if b & (LEFT | RIGHT) == 0 => '│',
        _ => '─',
    })
}

/// The lines of the tree on screen, as the directions each cell joins.
struct Lines {
    bits: Vec<u8>,
    /// The first row on screen.
    top: usize,
    height: usize,
    width: usize,
}

impl Lines {
    fn set(&mut self, x: usize, y: usize, b: u8) {
        if y >= self.top && y < self.top + self.height && x < self.width {
            self.bits[(y - self.top) * self.width + x] |= b;
        }
    }

    /// A branch along row `y` from the line at column `from` to `to`.
    fn branch(&mut self, from: usize, to: usize, y: usize, end: u8) {
        self.set(from, y, RIGHT);
        for x in from + 1..to {
            self.set(x, y, LEFT | RIGHT);
        }
        self.set(to, y, end);
    }
}

/// A length for the scale bar: the largest 1, 2 or 5 × 10^k of at most
/// `most`, written out.
fn bar_length(most: f64) -> Option<(f64, String)> {
    if most.is_nan() || most <= 0.0 || most.is_infinite() {
        return None;
    }
    let k = most.log10().floor() as i32;
    let unit = 10f64.powi(k);
    let m = [5.0, 2.0, 1.0]
        .into_iter()
        .find(|&m| m * unit <= most * (1.0 + 1e-9))
        .unwrap_or(1.0);
    let text = if k >= 0 {
        format!("{}", m * unit)
    } else {
        format!("0.{}{}", "0".repeat((-k - 1) as usize), m as u32)
    };
    Some((m * unit, text))
}

/// Draw the tree in `area`: `header` on the first row, then the rows on
/// screen, then the scale bar when there is one.
pub fn draw_tree(frame: &mut Frame, doc: &TreeDoc, view: &TreeView, header: &str, area: Rect) {
    let layout = tree_layout(area, doc, view);
    let buffer = frame.buffer_mut();
    if layout.header.height > 0 && area.width > 1 {
        let room = area.width as usize - 1;
        buffer.set_string(area.x + 1, area.y, fit(header, room), Style::new());
    }
    let g = geometry(doc, view, layout.branch_width as usize);
    let tree = doc.tree();
    let rows = layout.rows;
    let (top, height, width) = (view.top, rows.height as usize, rows.width as usize);
    let on_screen = |y: usize| y >= top && y < top + height;
    let mut lines = Lines {
        bits: vec![0u8; height * width],
        top,
        height,
        width,
    };
    let end = (top + height).min(view.rows.len());
    for y in top.min(end)..end {
        let (from, to) = g.rows[y];
        lines.branch(from, to, y, LEFT | RIGHT);
    }
    for (&n, o) in &g.open {
        let jy = view.junction.get(&n).copied().unwrap_or(o.first);
        lines.branch(o.from, o.x, jy, LEFT);
        for y in o.first.max(top)..=o.last.min((top + height).saturating_sub(1)) {
            let mut b = 0;
            if y > o.first {
                b |= UP;
            }
            if y < o.last {
                b |= DOWN;
            }
            lines.set(o.x, y, b);
        }
    }
    for (i, &b) in lines.bits.iter().enumerate() {
        if let Some(c) = line_char(b) {
            let (x, y) = ((i % width) as u16, (i / width) as u16);
            buffer[(rows.x + x, rows.y + y)].set_char(c);
        }
    }
    let at = |x: usize, y: usize| Position::new(rows.x + x as u16, rows.y + (y - top) as u16);
    let dim = Style::new().add_modifier(Modifier::DIM);
    // A support sits on its node's own branch, just before its end.
    let support = |n: NodeId, from: usize, to: usize, y: usize, buffer: &mut Buffer| {
        if let Some(text) = tree.support(n)
            && on_screen(y)
            && to < width
            && to.saturating_sub(from) > text.width()
        {
            buffer.set_string(at(to - text.width(), y).x, at(0, y).y, text, dim);
        }
    };
    for (&n, o) in &g.open {
        let jy = view.junction.get(&n).copied().unwrap_or(o.first);
        support(n, o.from, o.x, jy, buffer);
    }
    let matches = view
        .search
        .as_ref()
        .filter(|search| search.revision == doc.revision());
    for y in top.min(end)..end {
        let row = view.rows[y];
        let (from, to) = g.rows[y];
        if row.collapsed {
            support(row.node, from, to + 1, y, buffer);
        }
        let start = to + 2;
        if start >= width {
            continue;
        }
        let text = fit(&row_text(doc, view, &row), width - start);
        let mut style = Style::new();
        if g.selected
            .is_some_and(|(first, last)| (first..=last).contains(&y))
        {
            style = palette::SELECTED;
        }
        if !row.collapsed
            && matches
                .is_some_and(|search| search.inside.get(row.node as usize).is_some_and(|&k| k > 0))
        {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if row.node == view.cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let p = at(start, y);
        buffer.set_string(p.x, p.y, text, style);
    }
    if view.is_open(view.cursor)
        && let Some(o) = g.open.get(&view.cursor)
    {
        let jy = view.junction.get(&view.cursor).copied().unwrap_or(o.first);
        if on_screen(jy) && o.x < width {
            let p = at(o.x, jy);
            buffer[(p.x, p.y)].modifier.insert(Modifier::REVERSED);
        }
    }
    if layout.scale.height > 0
        && let Some(per) = g.scale
        && per > 0.0
        && let Some((length, text)) = bar_length(layout.branch_width as f64 / 4.0 / per)
    {
        let n = ((length * per).round() as usize).max(1);
        let mut bar = String::from("├");
        bar.push_str(&"─".repeat(n.saturating_sub(1)));
        bar.push('┤');
        bar.push(' ');
        bar.push_str(&text);
        let room = (layout.scale.width as usize).saturating_sub(1);
        if room > 0 {
            buffer.set_string(
                layout.scale.x + 1,
                layout.scale.y,
                fit(&bar, room),
                Style::new(),
            );
        }
    }
}

fn tips(n: usize) -> String {
    if n == 1 {
        "1 tip".to_string()
    } else {
        format!("{n} tips")
    }
}

/// `t.nwk · 500 tips · linked to a.fa (480 of 500) · showing 12 tips: A … B`.
pub fn header_text(file: &str, doc: &TreeDoc, view: &TreeView, linked: Option<&str>) -> String {
    let (tree, facts) = (doc.tree(), doc.facts());
    let mut text = format!("{file} · {}", tips(facts.total_tips()));
    if let (Some(name), Some(link)) = (linked, view.link) {
        text.push_str(&format!(
            " · linked to {name} ({} of {})",
            link.matched, link.tips
        ));
    }
    if view.shown != tree.root() {
        let name = |n| tree.label(n).unwrap_or_default();
        text.push_str(&format!(
            " · showing {}: {} … {}",
            tips(facts.tips(view.shown) as usize),
            name(facts.first_tip(view.shown)),
            name(facts.last_tip(view.shown))
        ));
    }
    text
}

/// A tip's name, or `▸ 12 tips  First … Last` for a collapsed clade (its
/// label instead of the ends when it has one that is not a support), with
/// `  (3 match)` when the search finds tips inside.
pub fn row_text(doc: &TreeDoc, view: &TreeView, row: &Row) -> String {
    let (tree, facts) = (doc.tree(), doc.facts());
    let n = row.node;
    if !row.collapsed {
        return tree.label(n).unwrap_or_default().to_string();
    }
    let name = |n| tree.label(n).unwrap_or_default();
    let mut text = match tree.label(n).filter(|_| tree.support(n).is_none()) {
        Some(label) => format!("▸ {}  {label}", tips(facts.tips(n) as usize)),
        None => format!(
            "▸ {}  {} … {}",
            tips(facts.tips(n) as usize),
            name(facts.first_tip(n)),
            name(facts.last_tip(n))
        ),
    };
    if let Some(search) = &view.search
        && search.revision == doc.revision()
        && let Some(&k) = search.inside.get(n as usize)
        && k > 0
    {
        text.push_str(&format!("  ({k} match)"));
    }
    text
}

/// The node at a screen cell: a row's node anywhere right of its parent's
/// line, an internal node on its vertical line or its own branch.
pub fn hit(
    layout: &TreeLayout,
    doc: &TreeDoc,
    view: &TreeView,
    column: u16,
    row: u16,
) -> Option<NodeId> {
    if !layout.rows.contains(Position::new(column, row)) {
        return None;
    }
    let x = (column - layout.rows.x) as usize;
    let y = view.top + (row - layout.rows.y) as usize;
    let g = geometry(doc, view, layout.branch_width as usize);
    for (&n, o) in &g.open {
        if x == o.x && (o.first..=o.last).contains(&y) {
            return Some(n);
        }
    }
    for (&n, o) in &g.open {
        if view.junction.get(&n) == Some(&y) && o.from < x && x <= o.x {
            return Some(n);
        }
    }
    let &(from, _) = g.rows.get(y)?;
    (x > from).then(|| view.rows[y].node)
}

/// The screen cell of a node: an internal node's junction, or the end of
/// a row's branch; `None` when it is not on screen.
pub fn node_position(
    layout: &TreeLayout,
    doc: &TreeDoc,
    view: &TreeView,
    n: NodeId,
) -> Option<(u16, u16)> {
    let g = geometry(doc, view, layout.branch_width as usize);
    let (x, y) = match g.open.get(&n) {
        Some(o) => (o.x, *view.junction.get(&n)?),
        None => {
            let i = view.rows.iter().position(|row| row.node == n)?;
            (g.rows[i].1, i)
        }
    };
    let rows = layout.rows;
    (y >= view.top && y < view.top + rows.height as usize && x < rows.width as usize)
        .then(|| (rows.x + x as u16, rows.y + (y - view.top) as u16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_view::TreeSearch;
    use crate::tree_view::tests::{Doc, T, doc, tip};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use unicode_width::UnicodeWidthStr;

    const T6: &str = T;

    /// The doc, a view fitted to the rows `area` leaves for them, and the
    /// layout.
    fn laid_out(text: &str, width: u16, height: u16) -> (Doc, TreeView, TreeLayout) {
        let doc = doc(text);
        let mut view = TreeView::new(doc.tree());
        let area = Rect::new(0, 0, width, height);
        let layout = tree_layout(area, doc.get(), &view);
        view.refit(doc.tree(), doc.facts(), layout.rows.height as usize);
        let layout = tree_layout(area, doc.get(), &view);
        (doc, view, layout)
    }

    fn render(doc: &Doc, view: &TreeView, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let header = header_text("t.nwk", doc.get(), view, None);
        terminal
            .draw(|frame| draw_tree(frame, doc.get(), view, &header, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                // A wide character's second cell holds a filler.
                let mut row = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draw(text: &str, width: u16, height: u16) -> String {
        draw_with(text, width, height, |_, _| {})
    }

    fn draw_with(
        text: &str,
        width: u16,
        height: u16,
        change: impl FnOnce(&TreeDoc, &mut TreeView),
    ) -> String {
        let (doc, mut view, _) = laid_out(text, width, height);
        change(doc.get(), &mut view);
        render(&doc, &view, width, height)
    }

    /// The tips matching `pattern`, counted into every clade above them.
    fn search(doc: &TreeDoc, pattern: &str) -> TreeSearch {
        let matches = doc.find_tips(pattern).unwrap();
        let mut inside = vec![0; doc.tree().preorder().len() * 2];
        for &m in &matches {
            let mut at = Some(m);
            while let Some(n) = at {
                inside[n as usize] += 1;
                at = doc.tree().parent(n);
            }
        }
        TreeSearch {
            pattern: pattern.to_string(),
            matches,
            inside,
            revision: doc.revision(),
        }
    }

    fn caterpillar_text(n: usize) -> String {
        let mut text = format!("t{}", n - 1);
        for i in (0..n - 1).rev() {
            text = format!("(t{i},{text})");
        }
        text + ";"
    }

    #[test]
    fn a_small_tree_draws_to_scale_with_supports_names_and_scale_bar() {
        let screen = draw("((A:1,B:2)90:3,(C:2,D:1)70:1,E:0.5);", 50, 9);
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn collapsed_clades_say_their_size_and_ends_and_matches() {
        let screen = draw_with(T6, 60, 4, |doc, v| v.search = Some(search(doc, "D|E")));
        assert!(screen.contains("▸ 3 tips  C … E  (2 match)"), "{screen}");
    }

    #[test]
    fn equal_lengths_or_no_lengths_draw_without_a_scale_bar() {
        let a = draw("((A,B),C);", 30, 6);
        let b = draw("((A:0,B:0):0,C:0);", 30, 6);
        assert_eq!(a, b, "no lengths and zero lengths both draw equal lengths");
        let bar = |l: &str| {
            l.split("┤ ")
                .nth(1)
                .is_some_and(|rest| rest.parse::<f64>().is_ok())
        };
        assert!(!a.lines().any(bar), "no scale bar:\n{a}");
        assert!(draw("((A:1,B:2):1,C:1);", 30, 6).lines().any(bar));
    }

    #[test]
    fn long_and_wide_names_are_cut_by_width_and_tiny_screens_do_not_panic() {
        let screen = draw(
            "(名前名前名前名前名前名前,BBBBBBBBBBBBBBBBBBBBBBBBBBBB);",
            20,
            5,
        );
        assert!(
            screen.lines().all(|l| l.width() <= 20) && screen.contains('…'),
            "{screen}"
        );
        for (w, h) in [(1, 1), (5, 2), (20, 3), (3, 20)] {
            draw(&caterpillar_text(50), w, h);
        }
    }

    #[test]
    fn a_click_hits_a_tip_a_collapsed_clade_or_an_internal_node() {
        let (doc, view, layout) = laid_out(T6, 40, 8);
        let doc = doc.get();
        let a = tip(doc.tree(), "A");
        let ab = doc.tree().parent(a).unwrap();
        let (x, y) = node_position(&layout, doc, &view, ab).unwrap();
        assert_eq!(hit(&layout, doc, &view, x, y), Some(ab));
        let (_, y) = node_position(&layout, doc, &view, a).unwrap();
        assert_eq!(
            hit(&layout, doc, &view, layout.rows.right() - 1, y),
            Some(a),
            "on its name"
        );
        assert_eq!(hit(&layout, doc, &view, 0, layout.header.y), None);
    }

    #[test]
    fn a_star_wider_than_the_screen_draws_the_rows_around_the_cursor() {
        let names: Vec<String> = (0..1000).map(|i| format!("t{i}")).collect();
        let text = format!("({});", names.join(","));
        let screen = draw_with(&text, 30, 12, |doc, v| {
            v.step(doc.tree(), doc.facts(), 500);
        });
        let lines: Vec<&str> = screen.lines().collect();
        assert_eq!(lines.len(), 12);
        assert!(lines.iter().any(|l| l.ends_with(" t999")), "{screen}");
        assert!(
            lines[1..]
                .iter()
                .all(|l| l.starts_with(" ├") || l.starts_with(" └")),
            "{screen}"
        );
    }
}
