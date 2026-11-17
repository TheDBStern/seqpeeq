//! Shifting residues in one sequence: inserting pushes the residues after
//! the column to the right, and removing pulls them to the left, in both
//! cases only up to the next gap, so the rest of the row stays where it is.

use super::View;

/// What an insert or remove does to one row: the new text from the
/// command's column on (everything after it is unchanged), and how many
/// gap columns the alignment must gain first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shift {
    pub bytes: Vec<u8>,
    pub added: usize,
}

/// `-` and `.` both mean a gap.
pub(crate) fn is_gap(byte: u8) -> bool {
    byte == b'-' || byte == b'.'
}

/// Insert `seq` (not empty) in front of `tail`, the row from the command's
/// column to its end. The residues up to the `k`-th gap of `tail` move `k`
/// to the right, `k` being the length of `seq`, and those gaps are used up.
/// Gaps the row lacks are added at the end first.
pub(crate) fn insert_into(tail: &[u8], seq: &[u8]) -> Shift {
    let k = seq.len();
    let gaps = tail.iter().filter(|&&b| is_gap(b)).count();
    let added = k.saturating_sub(gaps);
    // The index of the k-th gap of the tail extended by the added gaps.
    let end = match tail
        .iter()
        .enumerate()
        .filter(|&(_, &b)| is_gap(b))
        .nth(k - 1)
    {
        Some((at, _)) => at + 1,
        None => tail.len() + added,
    };
    let mut bytes = seq.to_vec();
    bytes.extend(tail[..end.min(tail.len())].iter().filter(|&&b| !is_gap(b)));
    Shift { bytes, added }
}

/// Remove the first `count` cells of `tail` (`1 <= count <= tail.len()`).
/// The residues after them, up to the next gap, move left, and the cells
/// they leave become gaps.
pub(crate) fn remove_from(tail: &[u8], count: usize) -> Vec<u8> {
    let rest = &tail[count..];
    let run = rest.iter().position(|&b| is_gap(b)).unwrap_or(rest.len());
    let mut bytes = rest[..run].to_vec();
    bytes.extend(std::iter::repeat_n(b'-', count));
    bytes
}

impl View {
    /// What inserting `seq` into `row` at `col` gives (0-based; `col` may be
    /// [`View::cols`], to add at the end). The caller checks bounds.
    pub fn plan_insert(&self, row: usize, col: usize, seq: &[u8]) -> Shift {
        insert_into(&self.tail(row, col), seq)
    }

    /// What removing `count` cells of `row` from `col` on gives (0-based;
    /// `col + count <= cols`). The caller checks bounds.
    pub fn plan_remove(&self, row: usize, col: usize, count: usize) -> Shift {
        Shift {
            bytes: remove_from(&self.tail(row, col), count),
            added: 0,
        }
    }

    /// The residues of `row` from `col` to its end.
    fn tail(&self, row: usize, col: usize) -> Vec<u8> {
        let mut tail = Vec::new();
        self.residues(row, col..self.cols(), &mut tail);
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msa::Alignment;
    use crate::msa::testing::{TRIO, run};
    use proptest::prelude::*;

    #[test]
    fn insert_moves_the_stretch_up_to_the_kth_gap() {
        for (tail, seq, bytes, added) in [
            ("GT-A--T", "AC", "ACGTA", 0),
            ("-GT", "A", "A", 0),    // fills the gap in front
            ("-GT", "-", "-", 0),    // a gap before a gap changes nothing
            ("GT-C", "-", "-GT", 0), // opens a gap
            ("G.T", "A", "AG", 0),   // '.' is a gap too
            ("G-T", "AC", "ACGT", 1),
            ("", "A", "A", 1),
        ] {
            assert_eq!(
                insert_into(tail.as_bytes(), seq.as_bytes()),
                Shift {
                    bytes: bytes.as_bytes().to_vec(),
                    added
                },
                "{tail} + {seq}"
            );
        }
    }

    #[test]
    fn remove_pulls_the_stretch_up_to_the_next_gap() {
        for (tail, count, bytes) in [
            ("ACGT-A", 2, "GT--"),
            ("A", 1, "-"),
            ("AGT", 1, "GT-"),
            ("AG.T", 1, "G-"),
            ("A-GT", 1, "-"),
        ] {
            assert_eq!(
                remove_from(tail.as_bytes(), count),
                bytes.as_bytes(),
                "{tail} - {count}"
            );
        }
    }

    #[test]
    fn plans_read_the_row_as_edited() {
        let mut aln = Alignment::from_text(TRIO);
        run(&mut aln, "delete-cols --cols 1").unwrap(); // s2 is now CGTTACG-A
        let view = aln.view();
        assert_eq!(
            view.plan_insert(1, 2, b"N"),
            Shift {
                bytes: b"NTTACG".to_vec(),
                added: 0
            }
        );
        assert_eq!(
            view.plan_remove(1, 0, 1),
            Shift {
                bytes: b"GTTACG-".to_vec(),
                added: 0
            }
        );
        assert_eq!(
            view.plan_insert(1, 9, b"AC"),
            Shift {
                bytes: b"AC".to_vec(),
                added: 2
            }
        );
    }

    fn ungapped(bytes: &[u8]) -> Vec<u8> {
        bytes.iter().copied().filter(|&b| !is_gap(b)).collect()
    }

    proptest! {
        #[test]
        fn shifts_keep_residue_order_and_touch_only_their_span(
            tail in proptest::collection::vec(proptest::sample::select(b"AC-.".to_vec()), 0..12),
            seq in proptest::collection::vec(proptest::sample::select(b"GT-".to_vec()), 1..4),
            count in any::<usize>(),
        ) {
            let Shift { bytes, added } = insert_into(&tail, &seq);
            let mut ext = tail.clone();
            ext.extend(std::iter::repeat_n(b'-', added));
            prop_assert!(bytes.len() <= ext.len());
            let mut new = bytes.clone();
            new.extend_from_slice(&ext[bytes.len()..]);
            prop_assert_eq!(new.len(), tail.len() + added);
            let mut residues = seq.clone();
            residues.extend_from_slice(&tail);
            prop_assert_eq!(ungapped(&new), ungapped(&residues));

            if !tail.is_empty() {
                let count = 1 + count % tail.len();
                let bytes = remove_from(&tail, count);
                prop_assert!(bytes.len() <= tail.len());
                let mut new = bytes.clone();
                new.extend_from_slice(&tail[bytes.len()..]);
                prop_assert_eq!(new.len(), tail.len());
                prop_assert_eq!(ungapped(&new), ungapped(&tail[count..]));
            }
        }
    }
}
