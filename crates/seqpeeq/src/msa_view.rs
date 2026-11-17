//! Where the user is in an alignment: cursor, scroll position and how it
//! is shown. Pure state: nothing here draws or reads the terminal.

use std::ops::Range;

use seqpeeq_core::{Alignment, MsaCmd, Ranges, RowSel, View};

use crate::zoom::Scale;

/// One residue's place: a row and a column, counted from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
}

/// A number of rows and columns: of an alignment, or of what fits on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub rows: usize,
    pub cols: usize,
}

impl Shape {
    /// The rows and columns of an alignment as it now is.
    pub fn of(alignment: &Alignment) -> Shape {
        Shape {
            rows: alignment.rows(),
            cols: alignment.cols(),
        }
    }
}

/// How residues are coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colouring {
    /// Shade the cells that carry their column's conserved residue.
    Conserved,
    /// A colour per kind of residue.
    Residue,
    Plain,
}

/// What a selection is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelKind {
    /// Some rows of some columns.
    Block,
    /// Whole sequences.
    Rows,
    /// Whole columns.
    Cols,
}

/// A selection in the making: it runs from `anchor` to the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Select {
    pub kind: SelKind,
    pub anchor: Cell,
}

/// Rows and columns, counted from 0 and half-open. `None` means all of
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub rows: Option<Range<usize>>,
    pub cols: Option<Range<usize>>,
}

impl Region {
    /// The flags that name this region in a command: `--rows` and `--cols`,
    /// counted from 1.
    pub fn flags(&self) -> (RowSel, Option<Ranges>) {
        let ranges = |range: &Range<usize>| Ranges::new(std::iter::once(range.clone()));
        let rows = RowSel {
            rows: self.rows.as_ref().and_then(ranges),
            ..RowSel::default()
        };
        (rows, self.cols.as_ref().and_then(ranges))
    }

    pub fn contains(&self, cell: Cell) -> bool {
        let within = |range: &Option<Range<usize>>, at: usize| {
            range.as_ref().is_none_or(|range| range.contains(&at))
        };
        within(&self.rows, cell.row) && within(&self.cols, cell.col)
    }
}

/// Characters typed over one row and not yet turned into a command. They
/// are always side by side, so the run is a start and the bytes from there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub row: usize,
    /// The column of the first typed character.
    pub start: usize,
    pub bytes: Vec<u8>,
}

impl Typed {
    pub fn new(row: usize, col: usize, byte: u8) -> Typed {
        Typed {
            row,
            start: col,
            bytes: vec![byte],
        }
    }

    /// Put `byte` at `col` if that is inside the run or right beside it.
    /// False, changing nothing, otherwise.
    pub fn put(&mut self, col: usize, byte: u8) -> bool {
        let end = self.start + self.bytes.len();
        if (self.start..end).contains(&col) {
            self.bytes[col - self.start] = byte;
        } else if col == end {
            self.bytes.push(byte);
        } else if col + 1 == self.start {
            self.bytes.insert(0, byte);
            self.start = col;
        } else {
            return false;
        }
        true
    }

    /// The command that makes the run part of the alignment.
    pub fn command(&self) -> MsaCmd {
        MsaCmd::Overwrite {
            row: self.row + 1,
            col: self.start + 1,
            seq: String::from_utf8_lossy(&self.bytes).into_owned(),
        }
    }
}

/// What has been typed and not yet turned into a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Characters typed over residues.
    Replace(Typed),
    /// Characters inserted at `col`, shifting what follows; `added` is
    /// how many gap columns the alignment gains for them at its end.
    Insert {
        row: usize,
        col: usize,
        seq: Vec<u8>,
        added: usize,
    },
    /// `count` cells removed from `col` on, by Delete or, when `back`, by
    /// Backspace.
    Remove {
        row: usize,
        col: usize,
        count: usize,
        back: bool,
    },
}

impl Pending {
    pub fn row(&self) -> usize {
        match self {
            Pending::Replace(typed) => typed.row,
            Pending::Insert { row, .. } | Pending::Remove { row, .. } => *row,
        }
    }

    /// The command that makes the run part of the alignment.
    pub fn command(&self) -> MsaCmd {
        match self {
            Pending::Replace(typed) => typed.command(),
            Pending::Insert { row, col, seq, .. } => MsaCmd::Insert {
                row: row + 1,
                col: col + 1,
                seq: String::from_utf8_lossy(seq).into_owned(),
            },
            Pending::Remove {
                row, col, count, ..
            } => MsaCmd::Remove {
                row: row + 1,
                col: col + 1,
                count: *count,
            },
        }
    }

    /// The gap columns the alignment gains when the run is made.
    pub fn added(&self) -> usize {
        match self {
            Pending::Insert { added, .. } => *added,
            _ => 0,
        }
    }

    /// How the row will read once the command runs: (row, first column,
    /// the bytes from there). Inserts and removes are planned by the same
    /// rule the commands use, so the preview is the result.
    pub fn preview(&self, view: &View) -> (usize, usize, Vec<u8>) {
        match self {
            Pending::Replace(typed) => (typed.row, typed.start, typed.bytes.clone()),
            Pending::Insert { row, col, seq, .. } => {
                (*row, *col, view.plan_insert(*row, *col, seq).bytes)
            }
            Pending::Remove {
                row, col, count, ..
            } => (*row, *col, view.plan_remove(*row, *col, *count).bytes),
        }
    }
}

/// A search of the sequence names, and what it found.
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    pub pattern: String,
    /// The rows whose names match, ascending.
    pub matches: Vec<usize>,
    /// The revision of the alignment the matches were found in. Rows move
    /// when the alignment changes, so the matches are only good for this.
    pub revision: u64,
}

impl Search {
    /// The next match from `row` on, wrapping round at the end: forwards
    /// or backwards, counting a match on `row` itself only if `inclusive`.
    /// Returns its place among the matches and its row.
    pub fn step(&self, row: usize, forward: bool, inclusive: bool) -> Option<(usize, usize)> {
        let last = self.matches.len().checked_sub(1)?;
        // How many matches lie before `row`, and whether one is on it.
        let before = self.matches.partition_point(|&at| at < row);
        let on = self.matches.get(before) == Some(&row);
        let index = match (forward, inclusive) {
            (true, true) => before,
            (true, false) => before + usize::from(on),
            (false, true) if on => before,
            (false, _) => before.checked_sub(1).unwrap_or(last),
        };
        let index = if index > last { 0 } else { index };
        Some((index, self.matches[index]))
    }

    /// Whether any row of `rows` matches.
    pub fn any_in(&self, rows: std::ops::Range<usize>) -> bool {
        let first = self.matches.partition_point(|&at| at < rows.start);
        self.matches.get(first).is_some_and(|&at| at < rows.end)
    }

    pub fn is_match(&self, row: usize) -> bool {
        self.matches.binary_search(&row).is_ok()
    }
}

/// The narrowest and widest the names column can be made, and the step.
const NAME_WIDTHS: (u16, u16, u16) = (4, 60, 4);

pub struct MsaView {
    pub cursor: Cell,
    /// The first row on screen.
    pub top: usize,
    /// The first column on screen.
    pub left: usize,
    /// Columns given to sequence names.
    pub name_width: u16,
    pub colouring: Colouring,
    /// The selection, if one has been started. Moving the cursor extends it.
    pub select: Option<Select>,
    /// What has been typed since the last command, while typing.
    pub typed: Option<Pending>,
    /// Inserting: the cursor may sit one past the last column, to add
    /// at the end of a row.
    pub append: bool,
    /// The last search of the names.
    pub search: Option<Search>,
    /// The row of the match last gone to. Lines of the overview hold
    /// several rows, so the cursor alone does not say which it was.
    pub found: Option<usize>,
    /// The zoom level, an index into the app's levels; 0 is the grid.
    pub zoom: usize,
    /// Sequences per line and columns per cell at that level, set by the
    /// app. [`Scale::ONE`] in the grid.
    pub scale: Scale,
}

impl MsaView {
    pub fn new() -> MsaView {
        MsaView {
            cursor: Cell::default(),
            top: 0,
            left: 0,
            name_width: 24,
            colouring: Colouring::Conserved,
            select: None,
            typed: None,
            append: false,
            search: None,
            found: None,
            zoom: 0,
            scale: Scale::ONE,
        }
    }

    /// What is selected: from the anchor to the cursor, both included,
    /// each end run out to the edge of its block and clipped to `aln`.
    pub fn region(&self, aln: Shape) -> Option<Region> {
        let Select { kind, anchor } = self.select?;
        let span = |a: usize, b: usize, per: usize, limit: usize| {
            let end = ((a.max(b) / per + 1) * per).min(limit);
            (a.min(b) / per * per).min(end)..end
        };
        let rows = span(anchor.row, self.cursor.row, self.scale.rows, aln.rows);
        let cols = span(anchor.col, self.cursor.col, self.scale.cols, aln.cols);
        Some(match kind {
            SelKind::Block => Region {
                rows: Some(rows),
                cols: Some(cols),
            },
            SelKind::Rows => Region {
                rows: Some(rows),
                cols: None,
            },
            SelKind::Cols => Region {
                rows: None,
                cols: Some(cols),
            },
        })
    }

    /// What an action applies to: the selection, or else the cursor's
    /// block (the cell under it, in the grid).
    pub fn target(&self, aln: Shape) -> Region {
        let Cell { row, col } = self.cursor;
        let Scale { rows, cols } = self.scale;
        self.region(aln).unwrap_or(Region {
            rows: Some(row..(row + rows).min(aln.rows)),
            cols: Some(col..(col + cols).min(aln.cols)),
        })
    }

    /// Move the cursor by whole blocks, stopping at the edges of the
    /// alignment.
    pub fn move_by(&mut self, rows: isize, cols: isize, aln: Shape) {
        let steps = |n: isize, per: usize| n.saturating_mul(per as isize);
        let row = self
            .cursor
            .row
            .saturating_add_signed(steps(rows, self.scale.rows));
        let col = self
            .cursor
            .col
            .saturating_add_signed(steps(cols, self.scale.cols));
        self.move_to(Some(row), Some(col), aln);
    }

    /// Put the cursor on a row, a column or both, stopping at the edges, at
    /// the start of its block.
    pub fn move_to(&mut self, row: Option<usize>, col: Option<usize>, aln: Shape) {
        if let Some(row) = row {
            self.cursor.row = row.min(aln.rows.saturating_sub(1));
        }
        if let Some(col) = col {
            // Inserting, the cursor may go one past the end, and past the
            // columns the run being typed adds.
            let last = if self.append {
                aln.cols + self.typed.as_ref().map_or(0, Pending::added)
            } else {
                aln.cols.saturating_sub(1)
            };
            self.cursor.col = col.min(last);
        }
        self.snap();
    }

    /// Put the cursor, the screen's corner and the selection's anchor on
    /// the starts of their blocks.
    pub fn snap(&mut self) {
        let Scale { rows, cols } = self.scale;
        let start = |at: usize, per: usize| at / per * per;
        self.cursor.row = start(self.cursor.row, rows);
        self.cursor.col = start(self.cursor.col, cols);
        self.top = start(self.top, rows);
        self.left = start(self.left, cols);
        if let Some(select) = &mut self.select {
            select.anchor.row = start(select.anchor.row, rows);
            select.anchor.col = start(select.anchor.col, cols);
        }
    }

    /// Shift the screen by lines and cells without moving the cursor, as
    /// the mouse wheel does. The last line and cell never go further than
    /// the screen's edge.
    pub fn scroll(&mut self, rows: isize, cols: isize, aln: Shape, screen: Shape) {
        let Scale {
            rows: per_line,
            cols: per_cell,
        } = self.scale;
        let shift = |first: usize, by: isize, total: usize, shown: usize, per: usize| {
            let most = total.div_ceil(per).saturating_sub(shown);
            (first / per).saturating_add_signed(by).min(most) * per
        };
        self.top = shift(self.top, rows, aln.rows, screen.rows, per_line);
        self.left = shift(self.left, cols, aln.cols, screen.cols, per_cell);
    }

    /// Shift the screen of `screen` lines and cells just enough to show the
    /// cursor.
    pub fn reveal(&mut self, screen: Shape) {
        let fit = |first: usize, at: usize, shown: usize, per: usize| {
            let (first, at) = (first / per, at / per);
            let line = if shown == 0 || at < first {
                at.min(first)
            } else if at >= first + shown {
                at + 1 - shown
            } else {
                first
            };
            line * per
        };
        self.top = fit(self.top, self.cursor.row, screen.rows, self.scale.rows);
        self.left = fit(self.left, self.cursor.col, screen.cols, self.scale.cols);
    }

    /// Scroll so that the cursor's block is in the middle of a screen of
    /// `screen` lines and cells, as far as the edges allow.
    pub fn centre(&mut self, screen: Shape, aln: Shape) {
        let middle = |at: usize, shown: usize, total: usize, per: usize| {
            let most = total.div_ceil(per).saturating_sub(shown);
            (at / per).saturating_sub(shown / 2).min(most) * per
        };
        self.top = middle(self.cursor.row, screen.rows, aln.rows, self.scale.rows);
        self.left = middle(self.cursor.col, screen.cols, aln.cols, self.scale.cols);
    }

    /// Pull the cursor and the screen back inside an alignment that may
    /// have shrunk.
    pub fn clamp(&mut self, aln: Shape) {
        self.move_to(Some(self.cursor.row), Some(self.cursor.col), aln);
        self.top = self.top.min(aln.rows.saturating_sub(1));
        let cols = aln.cols + self.typed.as_ref().map_or(0, Pending::added);
        self.left = self.left.min(cols.saturating_sub(1));
        // A selection anchored on a row or column that has gone means
        // nothing any more.
        if self
            .select
            .is_some_and(|select| select.anchor.row >= aln.rows || select.anchor.col >= aln.cols)
        {
            self.select = None;
        }
        self.snap();
    }

    pub fn cycle_colouring(&mut self) {
        self.colouring = match self.colouring {
            Colouring::Conserved => Colouring::Residue,
            Colouring::Residue => Colouring::Plain,
            Colouring::Plain => Colouring::Conserved,
        };
    }

    /// Make the names column wider (or, with a negative count, narrower).
    pub fn widen_names(&mut self, steps: i16) {
        let (narrowest, widest, step) = NAME_WIDTHS;
        let width = i32::from(self.name_width) + i32::from(steps) * i32::from(step);
        self.name_width = width.clamp(i32::from(narrowest), i32::from(widest)) as u16;
    }
}

/// Read what was typed at the go-to prompt: `27` is column 27, `12,27` is
/// row 12 and column 27, `12,` is row 12. Returns (row, column) counted
/// from 0.
pub fn parse_goto(text: &str) -> Result<(Option<usize>, Option<usize>), String> {
    let bad = || "go to: give a column (27), a row and column (12,27) or a row (12,)".to_string();
    // A position counted from 1, as typed; `None` for an empty part.
    let position = |part: &str| -> Result<Option<usize>, String> {
        match part.trim() {
            "" => Ok(None),
            digits => match digits.parse::<usize>() {
                Ok(n) if n > 0 => Ok(Some(n - 1)),
                _ => Err(bad()),
            },
        }
    };
    let found = match text.split_once(',') {
        None => (None, position(text)?),
        Some((row, col)) => (position(row)?, position(col)?),
    };
    if found == (None, None) {
        return Err(bad());
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALN: Shape = Shape {
        rows: 100,
        cols: 50,
    };
    const SCREEN: Shape = Shape { rows: 20, cols: 10 };

    #[test]
    fn the_cursor_stops_at_the_edges() {
        let mut view = MsaView::new();
        view.move_by(-1, -1, ALN);
        assert_eq!(view.cursor, Cell { row: 0, col: 0 });
        view.move_by(500, 500, ALN);
        assert_eq!(view.cursor, Cell { row: 99, col: 49 });
        view.move_to(Some(3), None, ALN);
        assert_eq!(view.cursor, Cell { row: 3, col: 49 });
        view.move_to(Some(1000), Some(1000), ALN);
        assert_eq!(view.cursor, Cell { row: 99, col: 49 });
    }

    #[test]
    fn zoomed_moves_go_by_blocks_and_snap_to_their_starts() {
        let aln = Shape {
            rows: 100,
            cols: 50,
        };
        let mut view = MsaView::new();
        view.cursor = Cell { row: 13, col: 7 };
        view.scale = Scale { rows: 4, cols: 8 };
        view.snap();
        assert_eq!(view.cursor, Cell { row: 12, col: 0 });
        view.move_by(1, 2, aln);
        assert_eq!(view.cursor, Cell { row: 16, col: 16 });
        view.move_by(100, 100, aln);
        assert_eq!(
            view.cursor,
            Cell { row: 96, col: 48 },
            "the last, partial blocks"
        );
    }

    #[test]
    fn a_zoomed_selection_covers_whole_blocks_up_to_the_edge() {
        let aln = Shape { rows: 10, cols: 50 };
        let mut view = MsaView::new();
        view.scale = Scale { rows: 4, cols: 8 };
        view.select = Some(Select {
            kind: SelKind::Block,
            anchor: Cell { row: 0, col: 8 },
        });
        view.cursor = Cell { row: 8, col: 40 };
        assert_eq!(
            view.region(aln),
            Some(Region {
                rows: Some(0..10),
                cols: Some(8..48)
            })
        );
        view.select = None;
        assert_eq!(
            view.target(aln),
            Region {
                rows: Some(8..10),
                cols: Some(40..48)
            }
        );
    }

    #[test]
    fn the_zoomed_screen_follows_by_blocks() {
        let aln = Shape {
            rows: 100,
            cols: 50,
        };
        let mut view = MsaView::new();
        view.scale = Scale { rows: 4, cols: 8 };
        view.cursor = Cell { row: 96, col: 0 };
        view.reveal(Shape { rows: 10, cols: 7 });
        assert_eq!(view.top, 60, "lines 15-24 of 25");
        view.centre(Shape { rows: 10, cols: 7 }, aln);
        assert_eq!(view.top, 60, "no further than the last line allows");
        view.cursor = Cell { row: 48, col: 0 };
        view.centre(Shape { rows: 10, cols: 7 }, aln);
        assert_eq!(view.top, 28, "line 12 shown sixth of ten");
        view.scroll(-100, 0, aln, Shape { rows: 10, cols: 7 });
        assert_eq!(view.top, 0);
    }

    #[test]
    fn append_lets_the_cursor_sit_one_past_the_last_column() {
        let mut view = MsaView::new();
        view.append = true;
        view.move_to(None, Some(usize::MAX), ALN);
        assert_eq!(view.cursor.col, 50);
        view.clamp(ALN);
        assert_eq!(view.cursor.col, 50);
        view.append = false;
        view.clamp(ALN);
        assert_eq!(view.cursor.col, 49);
    }

    #[test]
    fn a_pending_run_names_its_command() {
        use seqpeeq_core::Command;
        let line = |pending: Pending| Command::Msa(pending.command()).to_line().unwrap();
        assert_eq!(
            line(Pending::Insert {
                row: 2,
                col: 1,
                seq: b"NN".to_vec(),
                added: 0,
            }),
            "msa insert --row 3 --col 2 --seq NN"
        );
        assert_eq!(
            line(Pending::Remove {
                row: 0,
                col: 5,
                count: 2,
                back: true
            }),
            "msa remove --row 1 --col 6 --count 2"
        );
    }

    #[test]
    fn the_screen_follows_the_cursor_by_as_little_as_it_can() {
        let mut view = MsaView::new();
        view.move_by(0, 12, ALN);
        view.reveal(SCREEN);
        assert_eq!(view.left, 3);
        view.move_by(25, -11, ALN);
        view.reveal(SCREEN);
        assert_eq!((view.top, view.left), (6, 1));
        view.reveal(SCREEN);
        assert_eq!(
            (view.top, view.left),
            (6, 1),
            "already visible: nothing moves"
        );
        // A screen with no room shows nothing and moves nothing.
        view.reveal(Shape { rows: 0, cols: 0 });
        assert_eq!((view.top, view.left), (6, 1));
    }

    #[test]
    fn the_wheel_moves_the_screen_and_not_the_cursor() {
        let mut view = MsaView::new();
        view.scroll(3, 10, ALN, SCREEN);
        assert_eq!((view.top, view.left, view.cursor), (3, 10, Cell::default()));
        view.scroll(1000, 1000, ALN, SCREEN);
        assert_eq!(
            (view.top, view.left),
            (80, 40),
            "the last row and column stay on screen"
        );
        view.scroll(-1000, -1000, ALN, SCREEN);
        assert_eq!((view.top, view.left), (0, 0));
        // An alignment smaller than the screen never scrolls.
        view.scroll(5, 5, Shape { rows: 3, cols: 4 }, SCREEN);
        assert_eq!((view.top, view.left), (0, 0));
    }

    #[test]
    fn a_shrunken_alignment_pulls_everything_back_in() {
        let mut view = MsaView::new();
        view.move_by(99, 49, ALN);
        view.reveal(SCREEN);
        view.clamp(Shape { rows: 4, cols: 6 });
        assert_eq!(view.cursor, Cell { row: 3, col: 5 });
        assert!(view.top <= 3 && view.left <= 5);
    }

    #[test]
    fn name_width_and_colouring_step_within_their_limits() {
        let mut view = MsaView::new();
        assert_eq!(
            (view.name_width, view.colouring),
            (24, Colouring::Conserved)
        );
        view.widen_names(1);
        assert_eq!(view.name_width, 28);
        view.widen_names(-50);
        assert_eq!(view.name_width, 4);
        view.widen_names(50);
        assert_eq!(view.name_width, 60);
        view.cycle_colouring();
        assert_eq!(view.colouring, Colouring::Residue);
        view.cycle_colouring();
        assert_eq!(view.colouring, Colouring::Plain);
        view.cycle_colouring();
        assert_eq!(view.colouring, Colouring::Conserved);
    }

    #[test]
    fn go_to_accepts_three_forms() {
        assert_eq!(parse_goto("27"), Ok((None, Some(26))));
        assert_eq!(parse_goto(" 12 , 27 "), Ok((Some(11), Some(26))));
        assert_eq!(parse_goto("12,"), Ok((Some(11), None)));
        for bad in ["", "0", "x", "1,2,3", ",", "-4", "3,0", "0,3"] {
            assert_eq!(
                parse_goto(bad),
                Err(
                    "go to: give a column (27), a row and column (12,27) or a row (12,)"
                        .to_string()
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_selection_spans_anchor_and_cursor_in_either_direction() {
        let aln = Shape { rows: 10, cols: 10 };
        let mut view = MsaView::new();
        view.move_to(Some(4), Some(6), aln);
        assert_eq!(view.region(aln), None);
        assert_eq!(
            view.target(aln),
            Region {
                rows: Some(4..5),
                cols: Some(6..7)
            }
        );
        view.select = Some(Select {
            kind: SelKind::Block,
            anchor: view.cursor,
        });
        view.move_by(-2, -3, aln);
        let block = Region {
            rows: Some(2..5),
            cols: Some(3..7),
        };
        assert_eq!(view.region(aln), Some(block.clone()));
        assert_eq!(view.target(aln), block);
        view.select = Some(Select {
            kind: SelKind::Rows,
            anchor: Cell { row: 7, col: 0 },
        });
        assert_eq!(
            view.region(aln),
            Some(Region {
                rows: Some(2..8),
                cols: None
            })
        );
        view.select = Some(Select {
            kind: SelKind::Cols,
            anchor: Cell { row: 0, col: 3 },
        });
        assert_eq!(
            view.region(aln),
            Some(Region {
                rows: None,
                cols: Some(3..4)
            })
        );
    }

    #[test]
    fn a_region_names_itself_in_flags_counted_from_one() {
        let region = Region {
            rows: Some(1..4),
            cols: Some(17..30),
        };
        let (rows, cols) = region.flags();
        assert_eq!(rows.rows.unwrap().to_string(), "2-4");
        assert_eq!(cols.unwrap().to_string(), "18-30");
        let columns = Region {
            rows: None,
            cols: Some(0..1),
        };
        let (rows, cols) = columns.flags();
        assert!(rows.is_empty());
        assert_eq!(cols.unwrap().to_string(), "1");
        assert!(region.contains(Cell { row: 3, col: 17 }));
        assert!(!region.contains(Cell { row: 4, col: 17 }));
        assert!(!region.contains(Cell { row: 3, col: 30 }));
        assert!(columns.contains(Cell { row: 99, col: 0 }));
    }

    #[test]
    fn clamping_drops_a_selection_whose_anchor_is_gone() {
        let mut view = MsaView::new();
        view.select = Some(Select {
            kind: SelKind::Rows,
            anchor: Cell { row: 9, col: 0 },
        });
        view.clamp(Shape { rows: 3, cols: 3 });
        assert_eq!(view.select, None);
        view.select = Some(Select {
            kind: SelKind::Block,
            anchor: Cell { row: 2, col: 2 },
        });
        view.clamp(Shape { rows: 3, cols: 3 });
        assert!(view.select.is_some());
    }

    #[test]
    fn a_typed_run_grows_at_either_end_and_can_be_retyped() {
        use seqpeeq_core::Command;
        let mut typed = Typed::new(2, 5, b'A');
        assert!(typed.put(6, b'C') && typed.put(4, b'-') && typed.put(5, b'G'));
        assert_eq!(
            (typed.start, typed.bytes.as_slice()),
            (4, b"-GC".as_slice())
        );
        assert!(
            !typed.put(8, b'T') && !typed.put(2, b'T'),
            "not beside the run"
        );
        assert_eq!(
            Command::Msa(typed.command()).to_line().unwrap(),
            "msa overwrite --row 3 --col 5 --seq=-GC"
        );
    }

    #[test]
    fn stepping_through_matches_wraps_both_ways() {
        let search = Search {
            pattern: "x".into(),
            matches: vec![2, 5, 9],
            revision: 0,
        };
        assert_eq!(search.step(0, true, true), Some((0, 2)));
        assert_eq!(search.step(5, true, true), Some((1, 5)));
        assert_eq!(search.step(5, true, false), Some((2, 9)));
        assert_eq!(search.step(9, true, false), Some((0, 2)));
        assert_eq!(search.step(2, false, false), Some((2, 9)));
        assert_eq!(search.step(7, false, false), Some((1, 5)));
        assert!(search.is_match(5) && !search.is_match(6));
        assert!(search.any_in(3..6) && search.any_in(9..10));
        assert!(!search.any_in(6..9) && !search.any_in(3..3));
        let none = Search {
            pattern: "x".into(),
            matches: vec![],
            revision: 0,
        };
        assert_eq!(none.step(0, true, true), None);
    }
}
