//! Where the user is in an alignment: cursor, scroll position and how it
//! is shown. Pure state: nothing here draws or reads the terminal.

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

/// How residues are coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colouring {
    /// Shade the cells that carry their column's conserved residue.
    Conserved,
    /// A colour per kind of residue.
    Residue,
    Plain,
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
}

impl MsaView {
    pub fn new() -> MsaView {
        MsaView {
            cursor: Cell::default(),
            top: 0,
            left: 0,
            name_width: 24,
            colouring: Colouring::Conserved,
        }
    }

    /// Move the cursor, stopping at the edges of the alignment.
    pub fn move_by(&mut self, rows: isize, cols: isize, aln: Shape) {
        let row = self.cursor.row.saturating_add_signed(rows);
        let col = self.cursor.col.saturating_add_signed(cols);
        self.move_to(Some(row), Some(col), aln);
    }

    /// Put the cursor on a row, a column or both, stopping at the edges.
    pub fn move_to(&mut self, row: Option<usize>, col: Option<usize>, aln: Shape) {
        if let Some(row) = row {
            self.cursor.row = row.min(aln.rows.saturating_sub(1));
        }
        if let Some(col) = col {
            self.cursor.col = col.min(aln.cols.saturating_sub(1));
        }
    }

    /// Shift the screen without moving the cursor, as the mouse wheel does.
    /// The last row and column never go further than the screen's edge.
    pub fn scroll(&mut self, rows: isize, cols: isize, aln: Shape, screen: Shape) {
        let most = |total: usize, shown: usize| total.saturating_sub(shown);
        self.top = self
            .top
            .saturating_add_signed(rows)
            .min(most(aln.rows, screen.rows));
        self.left = self
            .left
            .saturating_add_signed(cols)
            .min(most(aln.cols, screen.cols));
    }

    /// Shift the screen just enough to show the cursor.
    pub fn reveal(&mut self, screen: Shape) {
        let fit = |first: usize, at: usize, shown: usize| {
            if shown == 0 || at < first {
                at.min(first)
            } else if at >= first + shown {
                at + 1 - shown
            } else {
                first
            }
        };
        self.top = fit(self.top, self.cursor.row, screen.rows);
        self.left = fit(self.left, self.cursor.col, screen.cols);
    }

    /// Pull the cursor and the screen back inside an alignment that may
    /// have shrunk.
    pub fn clamp(&mut self, aln: Shape) {
        self.move_to(Some(self.cursor.row), Some(self.cursor.col), aln);
        self.top = self.top.min(aln.rows.saturating_sub(1));
        self.left = self.left.min(aln.cols.saturating_sub(1));
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
}
