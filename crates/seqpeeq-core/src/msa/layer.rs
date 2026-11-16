//! The edits made to an alignment, kept apart from the file's bytes.
//!
//! Every edit changes only this layer, so its cost depends on the number of
//! rows and columns, never on their product, and undoing an edit is putting
//! back a copy of the layer.

use std::collections::HashMap;
use std::ops::Range;

use super::source::Source;
use crate::ranges::Ranges;

/// Every column of a file `len` columns wide.
fn whole(len: usize) -> Vec<Range<u32>> {
    std::iter::once(0..len as u32).collect()
}

/// A block overwritten with one byte. It is tied to records and to columns
/// of the file, so later deletions never have to rewrite it.
#[derive(Debug, Clone)]
struct Fill {
    /// Record ids, ascending.
    ids: Vec<u32>,
    /// Columns of the file, ascending.
    cols: Vec<Range<u32>>,
    with: u8,
}

/// Rows and columns passed in are current ones, counted from 0, and must be
/// in bounds; the caller checks.
#[derive(Debug, Clone)]
pub(crate) struct Layer {
    /// The record id behind each row, in display order.
    rows: Vec<u32>,
    /// The columns of the file still present, ascending. Aligned files only.
    cols: Vec<Range<u32>>,
    /// The current column at which each range of `cols` starts.
    col_starts: Vec<usize>,
    /// The current number of columns; for an unaligned file, the longest row.
    width: usize,
    /// The shortest row; equal to `width` when aligned.
    min_len: usize,
    aligned: bool,
    /// New names, by record id.
    names: HashMap<u32, String>,
    /// Overwritten blocks, oldest first.
    fills: Vec<Fill>,
}

impl Layer {
    pub(crate) fn new(source: &Source) -> Layer {
        let mut layer = Layer {
            rows: (0..source.rows() as u32).collect(),
            cols: Vec::new(),
            col_starts: Vec::new(),
            width: source.width(),
            min_len: source.min_len(),
            aligned: source.width() == source.min_len(),
            names: HashMap::new(),
            fills: Vec::new(),
        };
        if layer.aligned {
            layer.set_cols(whole(source.width()));
        }
        layer
    }

    pub(crate) fn rows(&self) -> usize {
        self.rows.len()
    }

    /// The number of columns; for an unaligned file, the longest row.
    pub(crate) fn cols(&self) -> usize {
        self.width
    }

    /// The length of the shortest row.
    pub(crate) fn min_len(&self) -> usize {
        self.min_len
    }

    pub(crate) fn is_aligned(&self) -> bool {
        self.aligned
    }

    /// The record id behind a row.
    pub(crate) fn id(&self, row: usize) -> usize {
        self.rows[row] as usize
    }

    pub(crate) fn name<'a>(&'a self, source: &'a Source, row: usize) -> &'a str {
        match self.names.get(&self.rows[row]) {
            Some(name) => name,
            None => source.name(self.id(row)),
        }
    }

    pub(crate) fn row_len(&self, source: &Source, row: usize) -> usize {
        if self.aligned {
            self.width
        } else {
            source.len(self.id(row))
        }
    }

    /// Append the residues of `row` in columns `cols` to `out`. Columns past
    /// the end of the row add nothing.
    pub(crate) fn read(&self, source: &Source, row: usize, cols: Range<usize>, out: &mut Vec<u8>) {
        let id = self.rows[row];
        if !self.aligned {
            // No column edits or fills exist while the file is unaligned.
            source.read(id as usize, cols, out);
            return;
        }
        let fills: Vec<&Fill> = self
            .fills
            .iter()
            .filter(|fill| fill.ids.binary_search(&id).is_ok())
            .collect();
        for segment in self.file_cols(&[cols]) {
            let at = out.len();
            source.read(
                id as usize,
                segment.start as usize..segment.end as usize,
                out,
            );
            for fill in &fills {
                for cols in &fill.cols {
                    let (start, end) = (cols.start.max(segment.start), cols.end.min(segment.end));
                    if start < end {
                        let from = at + (start - segment.start) as usize;
                        out[from..from + (end - start) as usize].fill(fill.with);
                    }
                }
            }
        }
    }

    pub(crate) fn delete_rows(&mut self, source: &Source, rows: &Ranges) {
        let mut kept = Vec::with_capacity(self.rows.len() - rows.count());
        let mut from = 0;
        for range in rows.as_slice() {
            kept.extend_from_slice(&self.rows[from..range.start]);
            from = range.end;
        }
        kept.extend_from_slice(&self.rows[from..]);
        self.rows = kept;

        if !self.aligned {
            let lengths = || self.rows.iter().map(|&id| source.len(id as usize));
            let (longest, shortest) = (lengths().max().unwrap_or(0), lengths().min().unwrap_or(0));
            self.width = longest;
            self.min_len = shortest;
            if longest == shortest {
                // What is left has one length, so column commands now apply.
                self.aligned = true;
                self.set_cols(whole(longest));
            }
        }
    }

    /// Aligned files only.
    pub(crate) fn delete_cols(&mut self, cols: &Ranges) {
        let gone = self.file_cols(cols.as_slice());
        let mut kept = Vec::with_capacity(self.cols.len() + gone.len());
        let mut gone = gone.iter().peekable();
        for range in &self.cols {
            let mut start = range.start;
            while let Some(cut) = gone.peek().filter(|cut| cut.start < range.end) {
                if cut.start > start {
                    kept.push(start..cut.start);
                }
                start = cut.end;
                gone.next();
            }
            if start < range.end {
                kept.push(start..range.end);
            }
        }
        self.set_cols(kept);
    }

    /// Aligned files only.
    pub(crate) fn fill(&mut self, rows: &Ranges, cols: &Ranges, with: u8) {
        let mut ids: Vec<u32> = rows
            .as_slice()
            .iter()
            .flat_map(|range| &self.rows[range.clone()])
            .copied()
            .collect();
        ids.sort_unstable();
        self.fills.push(Fill {
            ids,
            cols: self.file_cols(cols.as_slice()),
            with,
        });
    }

    pub(crate) fn rename(&mut self, row: usize, name: String) {
        self.names.insert(self.rows[row], name);
    }

    fn set_cols(&mut self, cols: Vec<Range<u32>>) {
        let mut start = 0;
        self.col_starts = cols
            .iter()
            .map(|range| {
                let at = start;
                start += (range.end - range.start) as usize;
                at
            })
            .collect();
        self.width = start;
        self.min_len = start;
        self.cols = cols;
    }

    /// The columns of the file behind current column ranges (ascending and
    /// not overlapping). Each result lies inside one range of `self.cols`,
    /// so a range deleted in between is never spanned.
    fn file_cols(&self, current: &[Range<usize>]) -> Vec<Range<u32>> {
        let mut out = Vec::new();
        for range in current {
            let end = range.end.min(self.width);
            if range.start >= end {
                continue;
            }
            // The last range of `cols` starting at or before `range.start`.
            let mut index = self.col_starts.partition_point(|&at| at <= range.start) - 1;
            let mut at = range.start;
            while at < end {
                let first = self.col_starts[index];
                let piece = &self.cols[index];
                let piece_len = (piece.end - piece.start) as usize;
                let take_end = end.min(first + piece_len);
                out.push(
                    piece.start + (at - first) as u32..piece.start + (take_end - first) as u32,
                );
                at = take_end;
                index += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msa::testing::{TRIO, grid};
    use crate::msa::{Alignment, Alphabet};
    use proptest::prelude::*;
    use std::path::Path;

    /// A source and its layer, with the layer's operations taking range text.
    struct T {
        source: Source,
        layer: Layer,
    }

    impl T {
        fn new(text: &str) -> T {
            let source = Source::from_vec(text.into(), Path::new("t.fa")).unwrap();
            let layer = Layer::new(&source);
            T { source, layer }
        }
        fn delete_rows(&mut self, rows: &str) {
            self.layer.delete_rows(&self.source, &rows.parse().unwrap());
        }
        fn delete_cols(&mut self, cols: &str) {
            self.layer.delete_cols(&cols.parse().unwrap());
        }
        fn fill(&mut self, rows: &str, cols: &str, with: u8) {
            self.layer
                .fill(&rows.parse().unwrap(), &cols.parse().unwrap(), with);
        }
        fn rename(&mut self, row: usize, name: &str) {
            self.layer.rename(row, name.to_string());
        }
        fn window(&self, row: usize, cols: Range<usize>) -> String {
            let mut out = Vec::new();
            self.layer.read(&self.source, row, cols, &mut out);
            String::from_utf8(out).unwrap()
        }
        fn row(&self, row: usize) -> String {
            self.window(row, 0..self.layer.cols())
        }
        fn name(&self, row: usize) -> &str {
            self.layer.name(&self.source, row)
        }
        fn cols(&self) -> usize {
            self.layer.cols()
        }
    }

    #[test]
    fn fresh_alignment_reads_like_the_file() {
        let a = Alignment::from_text(TRIO);
        assert_eq!((a.rows(), a.cols(), a.is_aligned()), (3, 10, true));
        assert_eq!(a.shape(), "3 sequences x 10 columns");
        assert_eq!(grid(&a)[2], ("s3 third one".into(), "AC--TACGTA".into()));
    }

    #[test]
    fn column_deletions_compose_in_current_coordinates() {
        let mut t = T::new(TRIO);
        t.delete_cols("2-3"); // s1 is now AT-ACGTA
        t.delete_cols("1,8"); // columns 1 and 8 of what is left
        assert_eq!((t.row(0), t.cols()), ("T-ACGT".to_string(), 6));
    }

    #[test]
    fn fills_follow_their_cells_through_later_deletions() {
        let mut t = T::new(TRIO);
        t.fill("1-2", "4-6", b'N');
        t.delete_cols("1-4");
        t.delete_rows("1");
        assert_eq!(
            (t.row(0), t.row(1)),
            ("NNCG-A".to_string(), "TACGTA".to_string())
        );
        t.fill("1", "1", b'-'); // the newer fill wins
        assert_eq!(t.row(0), "-NCG-A");
    }

    #[test]
    fn renames_stay_with_their_sequence() {
        let mut t = T::new(TRIO);
        t.rename(2, "third");
        t.delete_rows("1");
        assert_eq!([t.name(0), t.name(1)], ["s2", "third"]);
    }

    #[test]
    fn window_reads_match_full_reads() {
        let mut t = T::new(TRIO);
        t.delete_cols("3,7");
        assert_eq!(t.window(1, 2..5), t.row(1)[2..5]);
        assert_eq!(t.window(1, 6..99), t.row(1)[6..]);
    }

    #[test]
    fn ragged_file_becomes_aligned_when_the_odd_rows_go() {
        let mut a = Alignment::from_text(">a\nACGT\n>b\nAC\n>c\nTTGA\n");
        assert_eq!((a.is_aligned(), a.cols(), a.row_len(1)), (false, 4, 2));
        assert_eq!(a.shape(), "3 sequences, not aligned (lengths 2-4)");
        a.layer.delete_rows(&a.source, &"2".parse().unwrap());
        assert_eq!(
            (a.is_aligned(), a.shape()),
            (true, "2 sequences x 4 columns".to_string())
        );
    }

    #[test]
    fn shape_uses_the_singular_for_one() {
        assert_eq!(
            Alignment::from_text(">a\nA\n").shape(),
            "1 sequence x 1 column"
        );
    }

    #[test]
    fn alphabet_is_guessed_from_residues() {
        assert_eq!(Alignment::from_text(TRIO).alphabet(), Alphabet::Nucleotide);
        assert_eq!(
            Alignment::from_text(">p\nMKV-LAAGIW\n").alphabet(),
            Alphabet::Protein
        );
        assert_eq!(
            Alignment::from_text(">g\n----\n").alphabet(),
            Alphabet::Nucleotide
        );
    }

    /// An edit whose targets are fixed only when it is applied, so that it
    /// always lands inside the alignment as it then is.
    #[derive(Debug, Clone)]
    enum Op {
        DeleteRows(Vec<(usize, usize)>),
        DeleteCols(Vec<(usize, usize)>),
        Fill(Vec<(usize, usize)>, Vec<(usize, usize)>, u8),
        Rename(usize, String),
    }

    /// Turn seeds into ranges inside `0..limit`.
    fn pick(seeds: &[(usize, usize)], limit: usize) -> Ranges {
        Ranges::new(seeds.iter().map(|&(a, b)| {
            let start = a % limit;
            start..start + 1 + b % (limit - start)
        }))
        .unwrap()
    }

    fn seeds() -> impl Strategy<Value = Vec<(usize, usize)>> {
        proptest::collection::vec((any::<usize>(), any::<usize>()), 1..3)
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            seeds().prop_map(Op::DeleteRows),
            seeds().prop_map(Op::DeleteCols),
            (seeds(), seeds(), proptest::sample::select(b"N-x".to_vec()))
                .prop_map(|(rows, cols, with)| Op::Fill(rows, cols, with)),
            (any::<usize>(), "[a-z ]{1,6}").prop_map(|(row, name)| Op::Rename(row, name)),
        ]
    }

    fn matrix() -> impl Strategy<Value = Vec<Vec<u8>>> {
        (1..=6usize, 1..=12usize).prop_flat_map(|(rows, cols)| {
            let residue = proptest::sample::select(b"ACGT-acgn".to_vec());
            proptest::collection::vec(proptest::collection::vec(residue, cols), rows)
        })
    }

    proptest! {
        #[test]
        fn edits_match_a_plain_matrix(
            matrix in matrix(),
            wrap in 1..=13usize,
            ops in proptest::collection::vec(op(), 0..12),
            window in (any::<usize>(), any::<usize>(), any::<usize>()),
        ) {
            let mut text = String::new();
            for (index, row) in matrix.iter().enumerate() {
                text.push_str(&format!(">n{index}\n"));
                for line in row.chunks(wrap) {
                    text.push_str(std::str::from_utf8(line).unwrap());
                    text.push('\n');
                }
            }
            let mut t = T::new(&text);
            let mut model: Vec<(String, Vec<u8>)> = matrix
                .into_iter()
                .enumerate()
                .map(|(index, row)| (format!("n{index}"), row))
                .collect();

            for op in ops {
                let (rows, cols) = (model.len(), model[0].1.len());
                match op {
                    Op::DeleteRows(seeds) => {
                        let gone = pick(&seeds, rows);
                        if gone.count() == rows { continue; }
                        t.layer.delete_rows(&t.source, &gone);
                        let mut index = 0;
                        model.retain(|_| {
                            index += 1;
                            !gone.as_slice().iter().any(|r| r.contains(&(index - 1)))
                        });
                    }
                    Op::DeleteCols(seeds) => {
                        let gone = pick(&seeds, cols);
                        if gone.count() == cols { continue; }
                        t.layer.delete_cols(&gone);
                        for (_, row) in &mut model {
                            let mut index = 0;
                            row.retain(|_| {
                                index += 1;
                                !gone.as_slice().iter().any(|r| r.contains(&(index - 1)))
                            });
                        }
                    }
                    Op::Fill(row_seeds, col_seeds, with) => {
                        let (in_rows, in_cols) = (pick(&row_seeds, rows), pick(&col_seeds, cols));
                        t.layer.fill(&in_rows, &in_cols, with);
                        for r in in_rows.as_slice().iter().cloned().flatten() {
                            for c in in_cols.as_slice().iter().cloned().flatten() {
                                model[r].1[c] = with;
                            }
                        }
                    }
                    Op::Rename(row, name) => {
                        t.layer.rename(row % rows, name.clone());
                        model[row % rows].0 = name;
                    }
                }

                prop_assert_eq!(t.layer.rows(), model.len());
                prop_assert_eq!(t.cols(), model[0].1.len());
                for (index, (name, row)) in model.iter().enumerate() {
                    prop_assert_eq!(t.name(index), name);
                    prop_assert_eq!(t.row(index).into_bytes(), row.clone());
                }
                let row = window.0 % model.len();
                let start = window.1 % t.cols();
                let end = start + window.2 % (t.cols() - start + 1);
                prop_assert_eq!(
                    t.window(row, start..end).into_bytes(),
                    model[row].1[start..end].to_vec()
                );
            }
        }
    }
}
