//! What the zoomed-out overview shows: for blocks of columns of a row, how
//! many cells are residues and how many of those carry their column's
//! conserved residue.

use std::ops::Range;

use super::View;
use super::conservation::Conservation;
use super::shift::is_gap;
use crate::progress::Progress;

/// What a stretch of cells holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Positions covered.
    pub cells: u64,
    /// Cells that are not gaps.
    pub residues: u64,
    /// Residues equal to their column's conserved residue.
    pub matches: u64,
}

impl Tally {
    pub fn add(&mut self, other: Tally) {
        self.cells += other.cells;
        self.residues += other.residues;
        self.matches += other.matches;
    }

    /// More than half the cells are gaps.
    pub fn is_gappy(&self) -> bool {
        self.residues * 2 < self.cells
    }

    /// The share of residues that match, as a whole percentage rounded
    /// down; `None` when there are no residues.
    pub fn score(&self) -> Option<u8> {
        (self.residues > 0).then(|| (100 * self.matches / self.residues) as u8)
    }
}

/// The most memory a summary's tables may take.
pub const SUMMARY_BYTES: usize = 48 << 20;

/// The columns per stored block of a summary of `rows` x `cols`: the
/// smallest power of two that keeps its tables within [`SUMMARY_BYTES`].
/// `None` when even one block per sequence would not: there are too many
/// sequences to summarise.
pub fn summary_block(rows: usize, cols: usize) -> Option<usize> {
    let bytes = |block: usize| {
        (rows as u128)
            .saturating_mul(cols.div_ceil(block) as u128)
            .saturating_mul(4)
    };
    let mut block = 1usize;
    while bytes(block) > SUMMARY_BYTES as u128 {
        if block >= cols {
            return None;
        }
        block = block.checked_mul(2)?;
    }
    Some(block)
}

/// Tallies of every row of an alignment, in blocks of columns, for the
/// overview levels that put several sequences on one line.
#[derive(Debug, Clone)]
pub struct Summary {
    revision: u64,
    rows: usize,
    cols: usize,
    block: usize,
    blocks: usize,
    /// (residues, matches) per row and block, row by row.
    cells: Vec<[u16; 2]>,
    /// Each whole row.
    totals: Vec<Tally>,
}

impl Summary {
    /// The revision of the alignment summarised.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Columns per stored block.
    pub fn block(&self) -> usize {
        self.block
    }

    /// The sum over `rows` and stored `blocks`, each clipped to what there
    /// is.
    pub fn tally(&self, rows: Range<usize>, blocks: Range<usize>) -> Tally {
        let rows = rows.start.min(self.rows)..rows.end.min(self.rows);
        let blocks = blocks.start.min(self.blocks)..blocks.end.min(self.blocks);
        let mut tally = Tally::default();
        if rows.is_empty() || blocks.is_empty() {
            return tally;
        }
        let first = blocks.start * self.block;
        let last = (blocks.end * self.block).min(self.cols);
        tally.cells = ((last - first) * rows.len()) as u64;
        for row in rows {
            let at = row * self.blocks;
            for &[residues, matches] in &self.cells[at + blocks.start..at + blocks.end] {
                tally.residues += u64::from(residues);
                tally.matches += u64::from(matches);
            }
        }
        tally
    }

    /// The whole of one row.
    pub fn row(&self, row: usize) -> Tally {
        self.totals[row]
    }

    /// The memory held by its tables.
    pub fn bytes(&self) -> usize {
        self.cells.capacity() * std::mem::size_of::<[u16; 2]>()
            + self.totals.capacity() * std::mem::size_of::<Tally>()
    }
}

/// Add one cell to a tally.
fn count(tally: &mut Tally, col: usize, residue: u8, conserved: Option<&Conservation>) {
    tally.cells += 1;
    if !is_gap(residue) {
        tally.residues += 1;
        if conserved.is_some_and(|found| found.matches(col, residue)) {
            tally.matches += 1;
        }
    }
}

impl View {
    /// Tally `row` in cells of `per_cell` columns, over `cols` clipped to
    /// the alignment; the last cell may be narrower. Matches are counted
    /// only with a count of conservation as wide as the alignment.
    pub fn row_tallies(
        &self,
        row: usize,
        cols: Range<usize>,
        per_cell: usize,
        conserved: Option<&Conservation>,
        out: &mut Vec<Tally>,
    ) {
        out.clear();
        let cols = cols.start.min(self.cols())..cols.end.min(self.cols());
        let conserved = conserved.filter(|found| found.cols() == self.cols());
        let per_cell = per_cell.max(1);
        let mut residues = Vec::with_capacity(cols.len());
        self.residues(row, cols.clone(), &mut residues);
        for (index, chunk) in residues.chunks(per_cell).enumerate() {
            let first = cols.start + index * per_cell;
            let mut tally = Tally::default();
            for (offset, &residue) in chunk.iter().enumerate() {
                count(&mut tally, first + offset, residue, conserved);
            }
            out.push(tally);
        }
    }

    /// Tally every row, in blocks of [`summary_block`] columns. `None` if
    /// cancelled through `progress`, if the sequences differ in length, or
    /// if `conserved` is not as wide as the alignment.
    pub fn summary(&self, conserved: &Conservation, progress: &Progress) -> Option<Summary> {
        let block = summary_block(self.rows(), self.cols())?;
        self.summary_in_blocks(conserved, progress, block)
    }

    /// [`View::summary`], in blocks of `block` columns.
    pub(crate) fn summary_in_blocks(
        &self,
        conserved: &Conservation,
        progress: &Progress,
        block: usize,
    ) -> Option<Summary> {
        let (rows, cols) = (self.rows(), self.cols());
        if !self.is_aligned() || conserved.cols() != cols {
            return None;
        }
        let blocks = cols.div_ceil(block);
        progress.set_total(rows as u64);
        // Allocated once at full size, so the memory held is the budget.
        let mut cells = Vec::with_capacity(rows * blocks);
        let mut totals = Vec::with_capacity(rows);
        let mut residues = Vec::with_capacity(cols);
        for row in 0..rows {
            if progress.is_cancelled() {
                return None;
            }
            residues.clear();
            self.residues(row, 0..cols, &mut residues);
            let mut total = Tally {
                cells: cols as u64,
                ..Tally::default()
            };
            // The hot loop of the pass: every residue of the alignment
            // goes through it once.
            for (chunk, majority) in residues
                .chunks(block)
                .zip(conserved.majorities().chunks(block))
            {
                let (mut found, mut matched) = (0u32, 0u32);
                for (&residue, &letter) in chunk.iter().zip(majority) {
                    found += u32::from(!is_gap(residue));
                    // `letter` is an upper-case letter or 0; clearing the
                    // case bit maps only that letter, in either case, to it.
                    matched += u32::from(letter != 0 && residue & !0x20 == letter);
                }
                let narrow = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
                cells.push([narrow(found), narrow(matched)]);
                total.residues += u64::from(found);
                total.matches += u64::from(matched);
            }
            totals.push(total);
            progress.add(1);
        }
        Some(Summary {
            revision: self.revision(),
            rows,
            cols,
            block,
            blocks,
            cells,
            totals,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msa::Alignment;
    use crate::msa::testing::{measure, run};
    use crate::progress::Progress;
    use proptest::prelude::*;

    /// Conserved: A C G T A C G T (columns 3 and 7 by 2 of 4).
    const FOUR: &str = ">a\nACGTACGT\n>b\nACGTACGA\n>c\nAC--AC--\n>d\nTTTTTTTT\n";

    fn t(cells: u64, residues: u64, matches: u64) -> Tally {
        Tally {
            cells,
            residues,
            matches,
        }
    }

    #[test]
    fn a_row_is_tallied_block_by_block() {
        let aln = Alignment::from_text(FOUR);
        let conserved = aln.view().conservation(&Progress::new()).unwrap();
        let mut out = Vec::new();
        let view = aln.view();
        view.row_tallies(3, 0..8, 4, Some(&conserved), &mut out);
        assert_eq!(out, [t(4, 4, 1), t(4, 4, 1)]);
        view.row_tallies(2, 0..8, 4, Some(&conserved), &mut out);
        assert_eq!(out, [t(4, 2, 2), t(4, 2, 2)]);
        assert!(
            !out[0].is_gappy(),
            "exactly half gaps is not more than half"
        );
        view.row_tallies(1, 2..8, 4, None, &mut out);
        assert_eq!(
            out,
            [t(4, 4, 0), t(2, 2, 0)],
            "no count: no matches; the last block is short"
        );
        view.row_tallies(1, 0..99, 8, Some(&conserved), &mut out);
        assert_eq!((out.len(), out[0].score()), (1, Some(87)));
        assert!(t(4, 1, 0).is_gappy());
        assert_eq!(t(4, 0, 0).score(), None);
    }

    #[test]
    fn the_summary_block_keeps_the_tables_within_budget() {
        assert_eq!(summary_block(3, 10), Some(1));
        assert_eq!(summary_block(100_000, 30_000), Some(256));
        // Even one block per sequence is over budget: no summary at all.
        assert_eq!(summary_block(13_000_000, 100), None);
        assert_eq!(summary_block(usize::MAX, usize::MAX), None);
        for (rows, cols) in [(100_000, 30_000), (1_000, 1_000_000), (1, 1)] {
            let b = summary_block(rows, cols).unwrap();
            assert!(
                rows * cols.div_ceil(b) * 4 <= SUMMARY_BYTES,
                "{rows} x {cols}"
            );
            assert!(
                b == 1 || rows * cols.div_ceil(b / 2) * 4 > SUMMARY_BYTES,
                "the smallest"
            );
        }
    }

    #[test]
    fn a_summary_gives_scores_and_sums_of_blocks() {
        let aln = Alignment::from_text(FOUR);
        let conserved = aln.view().conservation(&Progress::new()).unwrap();
        let summary = aln
            .view()
            .summary_in_blocks(&conserved, &Progress::new(), 4)
            .unwrap();
        assert_eq!((summary.rows(), summary.cols(), summary.block()), (4, 8, 4));
        assert_eq!(summary.row(3).score(), Some(25));
        assert_eq!(summary.row(1).score(), Some(87));
        assert_eq!(summary.tally(2..4, 1..2), t(8, 6, 3));
        assert_eq!(summary.revision(), aln.revision());
    }

    #[test]
    fn a_summary_needs_a_count_of_the_same_width_and_stops_when_cancelled() {
        let mut aln = Alignment::from_text(FOUR);
        let conserved = aln.view().conservation(&Progress::new()).unwrap();
        let stop = Progress::new();
        stop.cancel();
        assert!(aln.view().summary(&conserved, &stop).is_none());
        run(&mut aln, "delete-cols --cols 1").unwrap();
        assert!(aln.view().summary(&conserved, &Progress::new()).is_none());
    }

    #[test]
    fn a_summary_holds_what_its_budget_says() {
        let text: String = (0..64)
            .map(|i| format!(">{i}\n{}\n", "ACGT-".repeat(2_000)))
            .collect();
        let aln = Alignment::from_text(&text);
        let conserved = aln.view().conservation(&Progress::new()).unwrap();
        let (summary, peak) =
            measure::peak_during(|| aln.view().summary(&conserved, &Progress::new()));
        let summary = summary.unwrap();
        assert!(
            peak <= summary.bytes() + (1 << 20),
            "held {peak}, tables {}",
            summary.bytes()
        );
        assert_eq!(
            summary.bytes(),
            64 * 10_000 * 4 + 64 * std::mem::size_of::<Tally>()
        );
    }

    proptest! {
        #[test]
        fn summaries_agree_with_row_tallies(
            (rows, cols, cells) in (1usize..=6, 1usize..=12).prop_flat_map(|(rows, cols)| {
                let cell = proptest::sample::select(b"ACGTacgt-.N".to_vec());
                (Just(rows), Just(cols), proptest::collection::vec(cell, rows * cols))
            }),
            block in 1usize..=5,
            spans in (any::<usize>(), any::<usize>(), any::<usize>(), any::<usize>()),
        ) {
            let mut text = String::new();
            for row in 0..rows {
                text.push_str(&format!(">{row}\n"));
                text.push_str(std::str::from_utf8(&cells[row * cols..(row + 1) * cols]).unwrap());
                text.push('\n');
            }
            let aln = Alignment::from_text(&text);
            let view = aln.view();
            let conserved = view.conservation(&Progress::new()).unwrap();
            let summary = view.summary_in_blocks(&conserved, &Progress::new(), block).unwrap();
            let blocks = cols.div_ceil(block);
            let r0 = spans.0 % rows;
            let r1 = r0 + 1 + spans.1 % (rows - r0);
            let b0 = spans.2 % blocks;
            let b1 = b0 + 1 + spans.3 % (blocks - b0);
            let span = b0 * block..(b1 * block).min(cols);
            let mut expected = Tally::default();
            let mut out = Vec::new();
            for row in r0..r1 {
                view.row_tallies(row, span.clone(), cols, Some(&conserved), &mut out);
                for tally in &out {
                    expected.add(*tally);
                }
            }
            prop_assert_eq!(summary.tally(r0..r1, b0..b1), expected);
            let mut whole = Tally::default();
            view.row_tallies(r0, 0..cols, cols, Some(&conserved), &mut out);
            for tally in &out {
                whole.add(*tally);
            }
            prop_assert_eq!(summary.row(r0), whole);
        }
    }
}
