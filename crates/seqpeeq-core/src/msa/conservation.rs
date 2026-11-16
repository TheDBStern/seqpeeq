//! Which residue, if any, most sequences share in each column.

use super::View;
use crate::progress::Progress;

/// For each column, the residue that at least half of the sequences carry,
/// if there is one.
///
/// Letters are compared without regard to case. Gap characters and anything
/// else that is not a letter count towards the number of sequences but are
/// never the conserved residue. Of two letters with exactly half each, the
/// one earlier in the alphabet is taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Conservation {
    revision: u64,
    /// The conserved residue of each column in upper case; 0 where none.
    majority: Vec<u8>,
}

impl Conservation {
    /// The [revision](super::Alignment::revision) of the alignment counted.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn cols(&self) -> usize {
        self.majority.len()
    }

    /// The conserved residue of a column, in upper case.
    pub fn majority(&self, col: usize) -> Option<u8> {
        self.majority
            .get(col)
            .copied()
            .filter(|&letter| letter != 0)
    }

    /// Whether `residue`, in either case, is the conserved one of `col`.
    /// False past the last column.
    pub fn matches(&self, col: usize, residue: u8) -> bool {
        self.majority(col) == Some(residue.to_ascii_uppercase())
    }
}

const LETTERS: usize = 26;

/// Columns counted together. The table of counters for one block takes
/// under 7 MB, however long the alignment.
const BLOCK: usize = 1 << 16;

impl View {
    /// Count every column. `None` if cancelled through `progress`, or if the
    /// sequences differ in length.
    ///
    /// This reads every residue once, so it takes time in proportion to the
    /// size of the alignment; memory stays small.
    pub fn conservation(&self, progress: &Progress) -> Option<Conservation> {
        self.conservation_in_blocks(progress, BLOCK)
    }

    /// [`View::conservation`], `block` columns at a time.
    fn conservation_in_blocks(&self, progress: &Progress, block: usize) -> Option<Conservation> {
        if !self.is_aligned() {
            return None;
        }
        let (rows, cols) = (self.rows(), self.cols());
        let blocks = cols.div_ceil(block);
        progress.set_total((rows * blocks) as u64);
        let mut majority = Vec::with_capacity(cols);
        let mut counts = vec![0u32; block.min(cols) * LETTERS];
        let mut residues = Vec::with_capacity(block.min(cols));
        for first in (0..cols).step_by(block) {
            let span = first..(first + block).min(cols);
            counts.fill(0);
            for row in 0..rows {
                if progress.is_cancelled() {
                    return None;
                }
                residues.clear();
                self.residues(row, span.clone(), &mut residues);
                for (col, residue) in residues.iter().enumerate() {
                    let letter = residue.to_ascii_uppercase().wrapping_sub(b'A') as usize;
                    if letter < LETTERS {
                        counts[col * LETTERS + letter] += 1;
                    }
                }
                progress.add(1);
            }
            majority.extend(counts.chunks(LETTERS).take(span.len()).map(|column| {
                // The first of the most frequent letters.
                let (mut letter, mut most) = (0, 0);
                for (index, &count) in column.iter().enumerate() {
                    if count > most {
                        (letter, most) = (index, count);
                    }
                }
                if most > 0 && most as usize * 2 >= rows {
                    b'A' + letter as u8
                } else {
                    0
                }
            }));
        }
        Some(Conservation {
            revision: self.revision(),
            majority,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msa::Alignment;
    use crate::msa::testing::{TRIO, run};
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    fn majorities(text: &str) -> String {
        let aln = Alignment::from_text(text);
        let found = aln.view().conservation(&Progress::new()).unwrap();
        (0..found.cols())
            .map(|col| found.majority(col).map_or('.', char::from))
            .collect()
    }

    #[test]
    fn a_column_is_conserved_when_half_the_sequences_agree() {
        assert_eq!(
            majorities(">1\nACGT-A\n>2\nACGA-C\n>3\nACTT-G\n>4\nAGGTTT\n"),
            "ACGT.."
        );
    }

    #[test]
    fn case_is_ignored_and_ties_take_the_earlier_letter() {
        assert_eq!(majorities(">1\nAC\n>2\naT\n>3\nGC\n>4\nGT\n"), "AC");
    }

    #[test]
    fn gaps_count_towards_the_total_but_are_never_conserved() {
        assert_eq!(majorities(">1\n-A\n>2\n-A\n>3\n-C\n>4\n.G\n"), ".A");
        assert_eq!(majorities(">1\n*1\n>2\n*1\n"), "..");
    }

    #[test]
    fn matches_compares_without_case_and_tolerates_any_column() {
        let aln = Alignment::from_text(">1\nAC\n>2\nAC\n");
        let found = aln.view().conservation(&Progress::new()).unwrap();
        assert!(found.matches(0, b'a') && found.matches(0, b'A'));
        assert!(!found.matches(0, b'C') && !found.matches(0, b'-') && !found.matches(9, b'A'));
    }

    /// Counts what this thread has allocated, so a test can see the most
    /// memory a call held at once.
    mod measure {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static LIVE: Cell<usize> = const { Cell::new(0) };
            static PEAK: Cell<usize> = const { Cell::new(0) };
        }

        pub struct Counting;

        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                let pointer = unsafe { System.alloc(layout) };
                if !pointer.is_null() {
                    let _ = LIVE.try_with(|live| {
                        live.set(live.get() + layout.size());
                        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(live.get())));
                    });
                }
                pointer
            }

            unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
                unsafe { System.dealloc(pointer, layout) };
                let _ = LIVE.try_with(|live| live.set(live.get().saturating_sub(layout.size())));
            }
        }

        #[global_allocator]
        static COUNTING: Counting = Counting;

        /// The most memory, beyond what was held at the start, that `f`
        /// held at once on this thread.
        pub fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
            let start = LIVE.with(Cell::get);
            PEAK.with(|peak| peak.set(start));
            let out = f();
            (out, PEAK.with(Cell::get) - start)
        }
    }

    #[test]
    fn a_very_long_alignment_is_counted_in_little_memory() {
        // One sequence of a million residues: a table of 26 counters per
        // column would take over 100 MB.
        let text = format!(">only\n{}\n", "ACGT".repeat(250_000));
        let aln = Alignment::from_text(&text);
        let (found, peak) = measure::peak_during(|| aln.view().conservation(&Progress::new()));
        let found = found.unwrap();
        assert_eq!(
            (found.cols(), found.majority(999_999)),
            (1_000_000, Some(b'T'))
        );
        assert!(peak < 16 << 20, "held {} MB at once", peak >> 20);
    }

    #[test]
    fn the_count_is_of_the_alignment_as_edited() {
        let mut aln = Alignment::from_text(TRIO);
        run(&mut aln, "delete-cols --cols 1-4").unwrap();
        run(&mut aln, "fill --rows 1-2 --cols 1 --with N").unwrap();
        let found = aln.view().conservation(&Progress::new()).unwrap();
        assert_eq!((found.cols(), found.revision()), (6, aln.revision()));
        // Column 1 is now N, N, T.
        assert_eq!(found.majority(0), Some(b'N'));
    }

    #[test]
    fn unaligned_or_cancelled_gives_nothing_and_progress_reaches_the_end() {
        let ragged = Alignment::from_text(">a\nAC\n>b\nACGT\n");
        assert!(ragged.view().conservation(&Progress::new()).is_none());
        let aln = Alignment::from_text(TRIO);
        let stop = Progress::new();
        stop.cancel();
        assert!(aln.view().conservation(&stop).is_none());
        let progress = Progress::new();
        assert_eq!(progress.fraction(), None);
        aln.view().conservation(&progress).unwrap();
        assert_eq!(progress.fraction(), Some(1.0));
    }

    proptest! {
        #[test]
        fn counting_matches_a_plain_tally(
            (rows, cols, cells) in (1usize..=8, 1usize..=12).prop_flat_map(|(rows, cols)| {
                let cell = proptest::sample::select(b"ACGTacgt-.N*".to_vec());
                (Just(rows), Just(cols), proptest::collection::vec(cell, rows * cols))
            })
        ) {
            let mut text = String::new();
            for row in 0..rows {
                text.push_str(&format!(">{row}\n"));
                text.push_str(std::str::from_utf8(&cells[row * cols..(row + 1) * cols]).unwrap());
                text.push('\n');
            }
            let aln = Alignment::from_text(&text);
            let found = aln.view().conservation(&Progress::new()).unwrap();
            for block in 1..=5 {
                let in_blocks = aln.view().conservation_in_blocks(&Progress::new(), block);
                prop_assert_eq!(in_blocks.as_ref(), Some(&found), "blocks of {}", block);
            }
            for col in 0..cols {
                let mut tally: BTreeMap<u8, usize> = BTreeMap::new();
                for row in 0..rows {
                    let cell = cells[row * cols + col].to_ascii_uppercase();
                    if cell.is_ascii_uppercase() {
                        *tally.entry(cell).or_default() += 1;
                    }
                }
                // The first of the most frequent letters, if it reaches half.
                let most = tally.values().copied().max().unwrap_or(0);
                let expected = tally
                    .iter()
                    .find(|&(_, &n)| n == most && n * 2 >= rows)
                    .map(|(&letter, _)| letter);
                prop_assert_eq!(found.majority(col), expected, "column {} of\n{}", col, text);
            }
        }
    }
}
