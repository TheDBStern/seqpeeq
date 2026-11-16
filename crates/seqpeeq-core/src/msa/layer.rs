//! The edits made to an alignment, kept apart from the file's bytes.
//!
//! Every edit changes only this layer, so its cost depends on the number of
//! rows and columns, never on their product, and undoing an edit is putting
//! back a copy of the layer.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use super::source::Source;
use crate::ranges::Ranges;

/// Every column of a file `len` columns wide.
fn whole(len: usize) -> Vec<Range<u32>> {
    std::iter::once(0..len as u32).collect()
}

/// What a fill writes.
#[derive(Debug, Clone)]
enum Paint {
    /// One byte in every cell.
    One(u8),
    /// One byte per column of the fill's `cols`, in order.
    Bytes(Arc<[u8]>),
}

/// A block overwritten. It is tied to records and to columns of the file,
/// so later deletions never have to rewrite it.
#[derive(Debug, Clone)]
struct Fill {
    /// Record ids, as ascending runs. A fill of whole columns is one run,
    /// however many rows there are.
    ids: Vec<Range<u32>>,
    /// Columns of the file, ascending. Columns at or past the source's
    /// width are gap columns added at the end, not in the file.
    cols: Vec<Range<u32>>,
    paint: Paint,
}

/// Rows and columns passed in are current ones, counted from 0, and must be
/// in bounds; the caller checks.
///
/// Every earlier state is kept for undo, so the parts are shared: a copy
/// costs a few pointers, and an edit replaces only the part it changes.
#[derive(Debug, Clone)]
pub(crate) struct Layer {
    /// The record id behind each row, in display order.
    rows: Arc<Vec<u32>>,
    /// The columns of the file still present, ascending. Aligned files only.
    cols: Arc<Vec<Range<u32>>>,
    /// The current column at which each range of `cols` starts.
    col_starts: Arc<Vec<usize>>,
    /// The current number of columns; for an unaligned file, the longest row.
    width: usize,
    /// The shortest row; equal to `width` when aligned.
    min_len: usize,
    aligned: bool,
    /// New names, by record id.
    names: Arc<HashMap<u32, String>>,
    /// Overwritten blocks, oldest first.
    fills: Vec<Arc<Fill>>,
    /// How many gap columns have been added at the end, deleted or not.
    /// The next one added is file column `source.width() + added`, so a
    /// column is never handed out twice.
    #[allow(dead_code)]
    added: u32,
    /// The number of this state, given by the alignment that owns it.
    pub(crate) revision: u64,
}

impl Layer {
    pub(crate) fn new(source: &Source) -> Layer {
        let mut layer = Layer {
            rows: Arc::new((0..source.rows() as u32).collect()),
            cols: Arc::default(),
            col_starts: Arc::default(),
            width: source.width(),
            min_len: source.min_len(),
            aligned: source.width() == source.min_len(),
            names: Arc::default(),
            fills: Vec::new(),
            added: 0,
            revision: 0,
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
            .map(Arc::as_ref)
            .filter(|fill| {
                let run = fill.ids.partition_point(|run| run.end <= id);
                fill.ids.get(run).is_some_and(|run| run.start <= id)
            })
            .collect();
        let width = source.width() as u32;
        for segment in self.file_cols(&[cols]) {
            let at = out.len();
            // Added columns are ranges of their own, so a segment is either
            // all in the file or all added.
            if segment.start >= width {
                out.resize(at + segment.len(), b'-');
            } else {
                source.read(
                    id as usize,
                    segment.start as usize..segment.end as usize,
                    out,
                );
            }
            for fill in &fills {
                // Where in a byte patch the current range of `cols` begins.
                let mut offset = 0;
                for cols in &fill.cols {
                    let (start, end) = (cols.start.max(segment.start), cols.end.min(segment.end));
                    if start < end {
                        let from = at + (start - segment.start) as usize;
                        let cells = &mut out[from..from + (end - start) as usize];
                        match &fill.paint {
                            Paint::One(with) => cells.fill(*with),
                            Paint::Bytes(bytes) => {
                                let first = offset + (start - cols.start) as usize;
                                cells.copy_from_slice(&bytes[first..first + cells.len()]);
                            }
                        }
                    }
                    offset += cols.len();
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
        self.rows = Arc::new(kept);

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
        for range in self.cols.iter() {
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
        let mut runs: Vec<Range<u32>> = Vec::new();
        for id in ids {
            match runs.last_mut() {
                Some(run) if run.end == id => run.end += 1,
                _ => runs.push(id..id + 1),
            }
        }
        self.fills.push(Arc::new(Fill {
            ids: runs,
            cols: self.file_cols(cols.as_slice()),
            paint: Paint::One(with),
        }));
    }

    /// Replace the residues of `row` from `col` on with `bytes`. Aligned
    /// files only. Stored as one patch, however varied the bytes.
    pub(crate) fn overwrite(&mut self, row: usize, col: usize, bytes: &[u8]) {
        let id = self.rows[row];
        let cols = self.file_cols(std::slice::from_ref(&(col..col + bytes.len())));
        self.fills.push(Arc::new(Fill {
            ids: std::iter::once(id..id + 1).collect(),
            cols,
            paint: Paint::Bytes(bytes.into()),
        }));
    }

    /// Add `n` columns of gaps at the end. Aligned files only.
    // Used by `msa insert`, next.
    #[allow(dead_code)]
    pub(crate) fn add_gap_cols(&mut self, source: &Source, n: usize) {
        let first = source.width() as u32 + self.added;
        self.added += n as u32;
        let mut cols = Vec::with_capacity(self.cols.len() + 1);
        cols.extend_from_slice(&self.cols);
        // Never merged into the range before it, so that file and added
        // columns are never read as one segment.
        cols.push(first..first + n as u32);
        self.set_cols(cols);
    }

    pub(crate) fn rename(&mut self, row: usize, name: String) {
        // Copies the names once if an earlier state still shares them.
        Arc::make_mut(&mut self.names).insert(self.rows[row], name);
    }

    fn set_cols(&mut self, cols: Vec<Range<u32>>) {
        let mut start = 0;
        let starts = cols
            .iter()
            .map(|range| {
                let at = start;
                start += (range.end - range.start) as usize;
                at
            })
            .collect();
        self.col_starts = Arc::new(starts);
        self.width = start;
        self.min_len = start;
        self.cols = Arc::new(cols);
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
    use std::sync::Arc;

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
    fn an_overwrite_is_one_patch_however_varied_its_bytes() {
        let mut t = T::new(TRIO);
        t.layer.overwrite(0, 2, b"TTAC-G");
        assert_eq!(t.layer.fills.len(), 1);
        assert_eq!(t.row(0), "ACTTAC-GTA");
        t.delete_cols("4-5"); // through the middle of the patch
        assert_eq!(t.row(0), "ACTC-GTA");
        assert_eq!(t.window(0, 2..5), "TC-");
    }

    #[test]
    fn added_gap_columns_read_as_gaps_and_take_edits_like_any_column() {
        let mut t = T::new(TRIO);
        t.layer.add_gap_cols(&t.source, 3);
        assert_eq!((t.cols(), t.row(1)), (13, "ACGTTACG-A---".to_string()));
        t.layer.overwrite(0, 9, b"AC");
        assert_eq!(t.row(0), "ACGT-ACGTAC--");
        t.delete_cols("11");
        assert_eq!(t.row(0), "ACGT-ACGTA--");
        // A column added later is a new one: the deleted one's edit stays gone.
        t.layer.add_gap_cols(&t.source, 1);
        assert_eq!(t.row(0), "ACGT-ACGTA---");
        assert_eq!(t.window(0, 9..13), "A---");
    }

    #[test]
    fn added_columns_lie_past_the_longest_record_even_after_it_is_deleted() {
        let mut t = T::new(">a\nACGTAA\n>b\nAC\n>c\nTT\n");
        t.delete_rows("1"); // now aligned, 2 wide; the file's widest record is 6
        t.layer.add_gap_cols(&t.source, 2);
        assert_eq!(
            (t.row(0), t.row(1)),
            ("AC--".to_string(), "TT--".to_string())
        );
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
        a.view
            .layer
            .delete_rows(&a.view.source, &"2".parse().unwrap());
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
        Overwrite(usize, usize, Vec<u8>),
        AddGapCols(usize),
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
            (
                any::<usize>(),
                any::<usize>(),
                proptest::collection::vec(proptest::sample::select(b"ACGT-n".to_vec()), 1..5)
            )
                .prop_map(|(row, col, bytes)| Op::Overwrite(row, col, bytes)),
            (1..=3usize).prop_map(Op::AddGapCols),
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
                    Op::Overwrite(row, col, bytes) => {
                        let (row, col) = (row % rows, col % cols);
                        let bytes = &bytes[..bytes.len().min(cols - col)];
                        t.layer.overwrite(row, col, bytes);
                        model[row].1[col..col + bytes.len()].copy_from_slice(bytes);
                    }
                    Op::AddGapCols(n) => {
                        t.layer.add_gap_cols(&t.source, n);
                        for (_, row) in &mut model {
                            row.extend(std::iter::repeat_n(b'-', n));
                        }
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

    #[test]
    fn an_edit_shares_what_it_does_not_change_with_earlier_states() {
        let mut t = T::new(TRIO);
        t.rename(0, "x");
        let before = t.layer.clone();
        t.fill("1-3", "2", b'N');
        assert!(Arc::ptr_eq(&before.rows, &t.layer.rows));
        assert!(Arc::ptr_eq(&before.cols, &t.layer.cols));
        assert!(Arc::ptr_eq(&before.names, &t.layer.names));
        let one_fill = t.layer.clone();
        t.fill("1", "1", b'-');
        assert!(Arc::ptr_eq(&one_fill.fills[0], &t.layer.fills[0]));
        t.delete_cols("1");
        assert!(Arc::ptr_eq(&before.rows, &t.layer.rows));
        assert!(Arc::ptr_eq(&before.names, &t.layer.names));
        assert_eq!((t.row(0), before.fills.len()), ("NGT-ACGTA".to_string(), 0));
    }

    #[test]
    fn a_fill_stores_runs_of_rows_not_every_row() {
        let text: String = (0..1000).map(|i| format!(">n{i}\nACGT\n")).collect();
        let mut t = T::new(&text);
        t.fill("1-1000", "2", b'N');
        assert_eq!(t.layer.fills[0].ids, vec![0..1000u32; 1]);
        t.delete_rows("500");
        t.fill("1-999", "3", b'x');
        assert_eq!(t.layer.fills[1].ids, [0..499, 500..1000]);
        assert_eq!(
            (t.row(0), t.row(499), t.row(998)),
            ("ANxT".into(), "ANxT".into(), "ANxT".into())
        );
    }
}
