//! The alignment grid: where its parts go, what is under a screen position,
//! and drawing it.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use seqpeeq_core::{Alignment, Conservation};
use unicode_width::UnicodeWidthChar;

use crate::msa_view::{Cell, Colouring, MsaView, Shape};
use crate::palette;

/// The fewest columns the residues keep when the names have to give way.
const MIN_CELLS: u16 = 10;

/// Where the parts of the grid are on screen. Any of them may be empty in
/// a terminal too small to show it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLayout {
    /// One row of column numbers, above the cells.
    pub ruler: Rect,
    /// Row numbers.
    pub numbers: Rect,
    /// Sequence names.
    pub names: Rect,
    /// The residues.
    pub cells: Rect,
}

/// Lay the grid out in `body` for an alignment of `rows` sequences, with
/// `name_width` columns wanted for names.
pub fn grid_layout(body: Rect, rows: usize, name_width: u16) -> GridLayout {
    let digits = rows.max(1).ilog10() as u16 + 1;
    // Each of the two left-hand parts ends in one blank column.
    let numbers = (digits + 1).min(body.width);
    let rest = body.width - numbers;
    let names = name_width
        .saturating_add(1)
        .min(rest.saturating_sub(MIN_CELLS));
    let ruler_height = body.height.min(1);
    let below = Rect {
        y: body.y + ruler_height,
        height: body.height - ruler_height,
        ..body
    };
    let cells = Rect {
        x: body.x + numbers + names,
        width: rest - names,
        ..below
    };
    GridLayout {
        ruler: Rect {
            y: body.y,
            height: ruler_height,
            ..cells
        },
        numbers: Rect {
            width: numbers,
            ..below
        },
        names: Rect {
            x: body.x + numbers,
            width: names,
            ..below
        },
        cells,
    }
}

/// How many rows and columns of residues the layout shows.
pub fn screen_shape(layout: &GridLayout) -> Shape {
    Shape {
        rows: layout.cells.height as usize,
        cols: layout.cells.width as usize,
    }
}

/// What a screen position is on.
#[derive(Debug, PartialEq)]
pub enum Hit {
    Cell(Cell),
    /// A sequence's name or number; holds the row.
    Name(usize),
    /// The ruler above a column; holds the column.
    Ruler(usize),
    Outside,
}

/// What is drawn at screen position (`x`, `y`). Drawing and the mouse both
/// go by the same layout, so they cannot disagree.
pub fn hit(layout: &GridLayout, view: &MsaView, aln: Shape, x: u16, y: u16) -> Hit {
    let cells = layout.cells;
    let col = (cells.left()..cells.right())
        .contains(&x)
        .then(|| view.left + (x - cells.x) as usize)
        .filter(|&col| col < aln.cols);
    if layout.ruler.contains(Position::new(x, y)) {
        return col.map_or(Hit::Outside, Hit::Ruler);
    }
    if !(cells.top()..cells.bottom()).contains(&y) {
        return Hit::Outside;
    }
    let row = view.top + (y - cells.y) as usize;
    if row >= aln.rows {
        return Hit::Outside;
    }
    let on_name =
        layout.numbers.contains(Position::new(x, y)) || layout.names.contains(Position::new(x, y));
    match col {
        _ if on_name => Hit::Name(row),
        Some(col) => Hit::Cell(Cell { row, col }),
        None => Hit::Outside,
    }
}

/// Where on screen the cursor's cell is, if it is on screen.
pub fn cursor_position(layout: &GridLayout, view: &MsaView) -> Option<Position> {
    let cells = layout.cells;
    let row = view.cursor.row.checked_sub(view.top)?;
    let col = view.cursor.col.checked_sub(view.left)?;
    (row < cells.height as usize && col < cells.width as usize)
        .then(|| Position::new(cells.x + col as u16, cells.y + row as u16))
}

/// `text` cut to `width` display columns, ending in `…` if anything was
/// cut. Control characters become spaces.
fn fit(text: &str, width: usize) -> String {
    let printable = |c: char| if c.is_control() { ' ' } else { c };
    let total: usize = text
        .chars()
        .map(|c| printable(c).width().unwrap_or(0))
        .sum();
    if total <= width {
        return text.chars().map(printable).collect();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars().map(printable) {
        let columns = c.width().unwrap_or(0);
        // Keep one column for the ellipsis.
        if used + columns + 1 > width {
            break;
        }
        out.push(c);
        used += columns;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Draw the alignment in `body`. Only the rows and columns on screen are
/// read, so the cost does not depend on the size of the alignment.
///
/// `conserved` is the count of columns to shade by, when one is ready.
pub fn draw_grid(
    frame: &mut Frame,
    alignment: &Alignment,
    view: &MsaView,
    conserved: Option<&Conservation>,
    body: Rect,
) {
    let layout = grid_layout(body, alignment.rows(), view.name_width);
    let GridLayout {
        ruler,
        numbers,
        names,
        cells,
    } = layout;
    let buffer = frame.buffer_mut();
    let dim = Style::new().add_modifier(Modifier::DIM);
    let selected = view.region();
    let is_selected = |cell: Cell| {
        selected
            .as_ref()
            .is_some_and(|region| region.contains(cell))
    };
    // Whole sequences and whole columns are marked on the names and on the
    // ruler as well as in the cells.
    let whole_rows = selected
        .as_ref()
        .is_some_and(|region| region.cols.is_none());
    let whole_cols = selected
        .as_ref()
        .is_some_and(|region| region.rows.is_none());

    // Column numbers: at column 1 and every tenth, each starting on its own
    // column, left out where it would run into the next or off the edge.
    let mut free_from = ruler.x;
    for offset in 0..ruler.width {
        let col = view.left + offset as usize;
        if col >= alignment.cols() {
            break;
        }
        let x = ruler.x + offset;
        let number = col + 1;
        if number == 1 || number.is_multiple_of(10) {
            let label = number.to_string();
            if x >= free_from && x + label.len() as u16 <= ruler.right() {
                buffer.set_string(x, ruler.y, &label, dim);
                free_from = x + label.len() as u16 + 1;
            }
        }
        // Marked after any number is written, so that a number running on
        // over the next column does not carry the mark with it.
        if whole_cols && is_selected(Cell { row: 0, col }) {
            buffer[(x, ruler.y)].set_style(palette::SELECTED);
        }
    }

    // A file whose sequences differ in length has no columns to compare.
    let colouring = match view.colouring {
        Colouring::Conserved if !alignment.is_aligned() => Colouring::Residue,
        chosen => chosen,
    };
    let mut residues = Vec::with_capacity(cells.width as usize);
    for line in 0..cells.height {
        let row = view.top + line as usize;
        if row >= alignment.rows() {
            break;
        }
        let y = cells.y + line;
        let marked = whole_rows && is_selected(Cell { row, col: 0 });
        if numbers.width > 1 {
            let width = numbers.width as usize - 1;
            let number = format!("{:>width$}", row + 1);
            buffer.set_stringn(numbers.x, y, number, width, dim);
        }
        if names.width > 1 {
            let width = names.width as usize - 1;
            let name = fit(alignment.name(row), width);
            // Matches are marked only while they are of the state shown.
            let found = view.search.as_ref().is_some_and(|search| {
                search.revision == alignment.revision() && search.is_match(row)
            });
            let style = if found {
                Style::new().add_modifier(Modifier::UNDERLINED)
            } else {
                Style::new()
            };
            buffer.set_stringn(names.x, y, name, width, style);
        }
        if marked {
            let label = Rect {
                y,
                height: 1,
                width: (numbers.width + names.width).saturating_sub(1),
                ..numbers
            };
            buffer.set_style(label, palette::SELECTED);
        }
        residues.clear();
        let on_screen = view.left..view.left + cells.width as usize;
        alignment.residues(row, on_screen, &mut residues);
        for (offset, &residue) in residues.iter().enumerate() {
            let col = view.left + offset;
            let style = match colouring {
                _ if is_selected(Cell { row, col }) => palette::SELECTED,
                Colouring::Plain => Style::new(),
                Colouring::Residue => palette::residue(alignment.alphabet(), residue),
                // Plain until the count is ready, and where it says no.
                Colouring::Conserved => match conserved {
                    Some(found) if found.matches(col, residue) => palette::SHADE,
                    _ => Style::new(),
                },
            };
            // A character typed and not yet made a command shows in place
            // of the residue it will replace, marked as provisional.
            let typed = view.typed.as_ref().and_then(|typed| typed.at(row, col));
            let (residue, style) = match typed {
                Some(typed) => (
                    typed,
                    style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
                None => (residue, style),
            };
            let shown = if residue.is_ascii_graphic() {
                residue as char
            } else {
                '?'
            };
            buffer[(cells.x + offset as u16, y)]
                .set_char(shown)
                .set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> GridLayout {
        // 5 sequences, names 24 wide, in an 80 x 10 body starting at row 1.
        grid_layout(Rect::new(0, 1, 80, 10), 5, 24)
    }

    #[test]
    fn the_parts_of_the_grid_do_not_overlap() {
        let l = layout();
        assert_eq!(l.numbers, Rect::new(0, 2, 2, 9));
        assert_eq!(l.names, Rect::new(2, 2, 25, 9));
        assert_eq!(l.cells, Rect::new(27, 2, 53, 9));
        assert_eq!(l.ruler, Rect::new(27, 1, 53, 1));
        assert_eq!(screen_shape(&l), Shape { rows: 9, cols: 53 });
        // A narrow body shrinks the names before the cells.
        let narrow = grid_layout(Rect::new(0, 1, 30, 10), 5, 24);
        assert_eq!(narrow.cells.width, 10);
        // A body with no room at all gives empty rectangles, not a panic.
        for (w, h) in [(0, 0), (1, 1), (3, 1), (5, 3)] {
            let tiny = grid_layout(Rect::new(0, 0, w, h), 100_000, 60);
            assert!(tiny.cells.right() <= w && tiny.cells.bottom() <= h);
            assert!(tiny.names.right() <= w && tiny.numbers.right() <= w);
        }
    }

    #[test]
    fn a_position_on_screen_maps_to_what_is_drawn_there() {
        let l = layout();
        let aln = Shape { rows: 5, cols: 60 };
        let mut view = MsaView::new();
        view.left = 7;
        view.top = 1;
        assert_eq!(
            hit(&l, &view, aln, 27, 2),
            Hit::Cell(Cell { row: 1, col: 7 })
        );
        assert_eq!(
            hit(&l, &view, aln, 30, 4),
            Hit::Cell(Cell { row: 3, col: 10 })
        );
        assert_eq!(hit(&l, &view, aln, 5, 3), Hit::Name(2));
        assert_eq!(
            hit(&l, &view, aln, 0, 3),
            Hit::Name(2),
            "the row number counts as the name"
        );
        assert_eq!(hit(&l, &view, aln, 29, 1), Hit::Ruler(9));
        assert_eq!(hit(&l, &view, aln, 5, 1), Hit::Outside, "left of the ruler");
        assert_eq!(
            hit(&l, &view, aln, 30, 7),
            Hit::Outside,
            "below the last sequence"
        );
        assert_eq!(hit(&l, &view, aln, 79, 0), Hit::Outside, "the tab bar");
        let short = Shape { rows: 5, cols: 9 };
        assert_eq!(
            hit(&l, &view, short, 40, 2),
            Hit::Outside,
            "right of the last column"
        );
        assert_eq!(
            cursor_position(&l, &view),
            None,
            "the cursor (0,0) is scrolled out of view"
        );
        view.cursor = Cell { row: 2, col: 9 };
        assert_eq!(cursor_position(&l, &view), Some(Position::new(29, 3)));
    }
}
