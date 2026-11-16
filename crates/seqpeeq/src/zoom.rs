//! Zoom levels: how many sequences share a line of the screen, and how many
//! columns share a cell. Pure arithmetic; nothing here draws.

use crate::msa_view::Shape;

/// Sequences per line and columns per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    pub rows: usize,
    pub cols: usize,
}

impl Scale {
    /// One residue per cell: the normal grid.
    pub const ONE: Scale = Scale { rows: 1, cols: 1 };
}

/// The zoom levels for an alignment of shape `aln` on a screen of `screen`
/// lines and cells, from the grid outwards. Columns are shrunk first, by
/// doubling, until they all fit; then rows, until they all fit too, with
/// at least `block` columns per cell (the summary's block, which lines of
/// several sequences are drawn from).
pub fn levels(aln: Shape, screen: Shape, block: usize) -> Vec<Scale> {
    let mut levels = vec![Scale::ONE];
    if screen.rows == 0 || screen.cols == 0 {
        return levels;
    }
    let mut cols = 1;
    while cols * screen.cols < aln.cols {
        cols *= 2;
        levels.push(Scale { rows: 1, cols });
    }
    let per_cell = cols.max(block);
    let mut rows = 1;
    while rows * screen.rows < aln.rows {
        rows *= 2;
        levels.push(Scale {
            rows,
            cols: per_cell,
        });
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(rows: usize, cols: usize) -> Scale {
        Scale { rows, cols }
    }

    #[test]
    fn columns_shrink_first_then_rows() {
        let big = levels(
            Shape {
                rows: 100_000,
                cols: 30_000,
            },
            Shape {
                rows: 30,
                cols: 150,
            },
            256,
        );
        assert_eq!(big.len(), 21);
        assert_eq!(&big[..3], [Scale::ONE, s(1, 2), s(1, 4)]);
        assert_eq!(big[8], s(1, 256), "the last column level fits every column");
        assert_eq!((big[9], big[20]), (s(2, 256), s(4096, 256)));
    }

    #[test]
    fn only_what_does_not_fit_is_shrunk() {
        let screen = Shape { rows: 20, cols: 50 };
        assert_eq!(levels(Shape { rows: 3, cols: 10 }, screen, 1), [Scale::ONE]);
        assert_eq!(
            levels(
                Shape {
                    rows: 500,
                    cols: 10
                },
                screen,
                1
            ),
            [Scale::ONE, s(2, 1), s(4, 1), s(8, 1), s(16, 1), s(32, 1)]
        );
        assert_eq!(
            levels(
                Shape {
                    rows: 3,
                    cols: 1000
                },
                screen,
                1
            )
            .last(),
            Some(&s(1, 32))
        );
        // Rows shrink in whole summary blocks, however few columns fit.
        let wide_screen = Shape {
            rows: 20,
            cols: 200,
        };
        assert_eq!(
            levels(
                Shape {
                    rows: 1000,
                    cols: 100
                },
                wide_screen,
                4
            )
            .last(),
            Some(&s(64, 4))
        );
        assert_eq!(
            levels(Shape { rows: 9, cols: 9 }, Shape { rows: 0, cols: 0 }, 1),
            [Scale::ONE]
        );
    }
}
