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

impl View {
    /// Count every column. `None` if cancelled through `progress`, or if the
    /// sequences differ in length.
    ///
    /// This reads every residue once, row by row, so it takes time in
    /// proportion to the size of the alignment.
    pub fn conservation(&self, progress: &Progress) -> Option<Conservation> {
        if !self.is_aligned() {
            return None;
        }
        let (rows, cols) = (self.rows(), self.cols());
        progress.set_total(rows as u64);
        let mut counts = vec![0u32; cols * LETTERS];
        let mut residues = Vec::with_capacity(cols);
        for row in 0..rows {
            if progress.is_cancelled() {
                return None;
            }
            residues.clear();
            self.residues(row, 0..cols, &mut residues);
            for (col, residue) in residues.iter().enumerate() {
                let letter = residue.to_ascii_uppercase().wrapping_sub(b'A') as usize;
                if letter < LETTERS {
                    counts[col * LETTERS + letter] += 1;
                }
            }
            progress.add(1);
        }
        let majority = counts
            .chunks(LETTERS)
            .map(|column| {
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
            })
            .collect();
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
