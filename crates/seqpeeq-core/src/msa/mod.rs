//! Alignments: a FASTA file read in place, under a layer of edits.

pub(crate) mod conservation;
pub(crate) mod layer;
pub(crate) mod source;
pub(crate) mod write;

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::Regex;

pub use conservation::Conservation;

use crate::command::msa::{MsaCmd, RowSel};
use crate::ranges::Ranges;
use crate::{Error, Result};
use layer::Layer;
use source::Source;

/// What the residues are, which decides how they are coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alphabet {
    Nucleotide,
    Protein,
}

/// An open alignment: the file as it is on disk, seen through the edits made
/// so far. Rows and columns are counted from 0 here; commands and messages
/// count from 1.
pub struct Alignment {
    view: View,
    alphabet: Alphabet,
    /// Earlier states and the command that replaced each, oldest first.
    undo: Vec<(Layer, &'static str)>,
    /// States that were undone, most recently undone last.
    redo: Vec<(Layer, &'static str)>,
    /// The number the next new state will get.
    next_revision: u64,
    /// The revision that was last written to a file, or opened.
    written: u64,
}

/// An alignment at one moment: the file plus the edits made by then. Cheap
/// to clone, and safe to read from another thread.
#[derive(Clone)]
pub struct View {
    source: Arc<Source>,
    layer: Layer,
}

impl View {
    pub fn rows(&self) -> usize {
        self.layer.rows()
    }

    /// The number of columns; for an unaligned file, the longest sequence.
    pub fn cols(&self) -> usize {
        self.layer.cols()
    }

    /// Whether every sequence has the same length.
    pub fn is_aligned(&self) -> bool {
        self.layer.is_aligned()
    }

    pub fn name(&self, row: usize) -> &str {
        self.layer.name(&self.source, row)
    }

    /// The length of one row; the same as [`View::cols`] when aligned.
    pub fn row_len(&self, row: usize) -> usize {
        self.layer.row_len(&self.source, row)
    }

    /// Append the residues of `row` in columns `cols` to `out`. Columns past
    /// the end of the row add nothing.
    pub fn residues(&self, row: usize, cols: Range<usize>, out: &mut Vec<u8>) {
        self.layer.read(&self.source, row, cols, out);
    }

    /// The number of this state; see [`Alignment::revision`].
    pub fn revision(&self) -> u64 {
        self.layer.revision
    }
}

/// How many changes can be undone.
pub const UNDO_LEVELS: usize = 20;

/// What a command did, for the status line and the log.
#[derive(Debug)]
pub(crate) struct Applied {
    pub message: String,
    /// A comment to write to the log after the command.
    pub note: Option<String>,
    /// A file the command created.
    pub created: Option<PathBuf>,
}

impl Applied {
    fn new(message: String) -> Applied {
        Applied {
            message,
            note: None,
            created: None,
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

fn regex(pattern: &str) -> Result<Regex> {
    // The library's own message runs over several lines.
    Regex::new(pattern).or_else(|_| invalid(format!("bad regular expression '{pattern}'")))
}

/// Refuse a replacement that names a group the pattern does not have. The
/// regex library would silently put nothing there, and it reads `$2_` as a
/// group called `2_`, so the common `$2_$1` would quietly delete text.
fn check_replacement(re: &Regex, replace: &str) -> Result<()> {
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let exists = |name: &str| match name.parse::<usize>() {
        Ok(index) => index < re.captures_len(),
        Err(_) => re.capture_names().flatten().any(|group| group == name),
    };
    let mut rest = replace;
    while let Some(at) = rest.find('$') {
        rest = &rest[at + 1..];
        if let Some(after) = rest.strip_prefix('$') {
            // `$$` is a literal dollar sign.
            rest = after;
            continue;
        }
        let (name, braced) = match rest.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], true),
                None => continue,
            },
            None => {
                let end = rest.find(|c| !is_name(c)).unwrap_or(rest.len());
                (&rest[..end], false)
            }
        };
        if name.is_empty() || exists(name) {
            continue;
        }
        let mut message =
            format!("--replace refers to group '{name}', which the pattern does not have");
        let digits = name.len() - name.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (number, text) = name.split_at(digits);
        if !braced && !number.is_empty() && !text.is_empty() && exists(number) {
            message.push_str(&format!(
                "; write ${{{number}}}{text} for group {number} followed by '{text}'"
            ));
        }
        return invalid(message);
    }
    Ok(())
}

const NOT_ALIGNED: &str = "sequences differ in length, so columns cannot be selected";

// Not derived: the contents can be gigabytes.
impl std::fmt::Debug for Alignment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Alignment({})", self.shape())
    }
}

impl Alignment {
    /// Open a FASTA file. Plain files are read in place; gzip is decompressed
    /// into memory.
    pub fn load(path: &Path) -> Result<Alignment> {
        Ok(Alignment::new(Source::load(path)?))
    }

    fn new(source: Source) -> Alignment {
        let alphabet = guess_alphabet(&source);
        Alignment {
            view: View {
                layer: Layer::new(&source),
                source: Arc::new(source),
            },
            alphabet,
            undo: Vec::new(),
            redo: Vec::new(),
            next_revision: 1,
            written: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_text(text: &str) -> Alignment {
        Alignment::new(Source::from_vec(text.into(), Path::new("test.fa")).unwrap())
    }

    /// The alignment as it is now. Clone it to keep this state, or to read
    /// it from another thread.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// A number for the current state: 0 as opened, a new number after each
    /// edit. Undo and redo bring back the number the state had.
    pub fn revision(&self) -> u64 {
        self.view.revision()
    }

    /// Whether the alignment differs from what was opened or last written
    /// to a file.
    pub fn is_modified(&self) -> bool {
        self.revision() != self.written
    }

    /// Record that the current state has been written to a file.
    pub(crate) fn mark_written(&mut self) {
        self.written = self.revision();
    }

    /// The rows whose name matches `pattern`, a regular expression with
    /// letter case ignored. Ascending; empty when nothing matches.
    pub fn find_names(&self, pattern: &str) -> Result<Vec<usize>> {
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .or_else(|_| invalid(format!("bad regular expression '{pattern}'")))?;
        Ok((0..self.rows())
            .filter(|&row| re.is_match(self.name(row)))
            .collect())
    }

    pub fn rows(&self) -> usize {
        self.view.rows()
    }

    /// The number of columns; for an unaligned file, the longest sequence.
    pub fn cols(&self) -> usize {
        self.view.cols()
    }

    /// Whether every sequence has the same length.
    pub fn is_aligned(&self) -> bool {
        self.view.is_aligned()
    }

    pub fn alphabet(&self) -> Alphabet {
        self.alphabet
    }

    pub fn name(&self, row: usize) -> &str {
        self.view.name(row)
    }

    /// The length of one row; the same as [`Alignment::cols`] when aligned.
    pub fn row_len(&self, row: usize) -> usize {
        self.view.row_len(row)
    }

    /// Append the residues of `row` in columns `cols` to `out`. Columns past
    /// the end of the row add nothing.
    pub fn residues(&self, row: usize, cols: Range<usize>, out: &mut Vec<u8>) {
        self.view.residues(row, cols, out);
    }

    /// The size in words, such as `3 sequences x 10 columns`.
    pub fn shape(&self) -> String {
        let sequences = count(self.rows(), "sequence");
        if self.is_aligned() {
            format!("{sequences} x {}", count(self.cols(), "column"))
        } else {
            format!(
                "{sequences}, not aligned (lengths {}-{})",
                self.view.layer.min_len(),
                self.cols()
            )
        }
    }
}

/// Commands. Everything is checked before anything is changed, so a refused
/// command leaves the alignment exactly as it was.
impl Alignment {
    pub(crate) fn apply(&mut self, cmd: &MsaCmd) -> Result<Applied> {
        match cmd {
            MsaCmd::DeleteRows { rows } => self.delete_rows(rows),
            MsaCmd::DeleteCols { cols } => self.delete_cols(cols),
            MsaCmd::Fill { rows, cols, with } => self.fill(rows, cols.as_ref(), *with),
            MsaCmd::Overwrite { row, col, seq } => self.overwrite(*row, *col, seq),
            MsaCmd::Rename {
                rows, to: Some(to), ..
            } => self.rename_one(rows, to),
            MsaCmd::Rename {
                rows,
                regex: Some(pattern),
                replace: Some(replace),
                ..
            } => self.rename_matching(rows, pattern, replace),
            MsaCmd::Rename { .. } => {
                invalid("say the new name with --to, or use --regex with --replace")
            }
            MsaCmd::Extract {
                rows,
                cols,
                wrap,
                out,
            } => self.extract(rows, cols.as_ref(), *wrap, out),
        }
    }

    pub(crate) fn undo(&mut self) -> Result<Applied> {
        let Some((layer, name)) = self.undo.pop() else {
            return invalid("nothing to undo");
        };
        let undone = std::mem::replace(&mut self.view.layer, layer);
        self.redo.push((undone, name));
        Ok(Applied::new(format!("undid {name}; now {}", self.shape())))
    }

    pub(crate) fn redo(&mut self) -> Result<Applied> {
        let Some((layer, name)) = self.redo.pop() else {
            return invalid("nothing to redo");
        };
        let before = std::mem::replace(&mut self.view.layer, layer);
        self.undo.push((before, name));
        Ok(Applied::new(format!("redid {name}; now {}", self.shape())))
    }

    /// The edits as they are now, for putting back with [`Alignment::restore`].
    pub(crate) fn snapshot(&self) -> Layer {
        self.view.layer.clone()
    }

    /// Put back a snapshot. The undo history is left as it is.
    pub(crate) fn restore(&mut self, layer: Layer) {
        self.view.layer = layer;
    }

    /// The rows a selection names, counted from 0, ascending. An empty
    /// selection is every row.
    pub(crate) fn select_rows(&self, sel: &RowSel) -> Result<Vec<usize>> {
        if let Some(rows) = &sel.rows {
            if rows.end() > self.rows() {
                return invalid(format!(
                    "row {} is past the last row ({})",
                    rows.end(),
                    self.rows()
                ));
            }
            return Ok(rows.as_slice().iter().cloned().flatten().collect());
        }
        if !sel.name.is_empty() {
            let mut found = Vec::new();
            for name in &sel.name {
                let before = found.len();
                found.extend((0..self.rows()).filter(|&row| self.name(row) == name));
                if found.len() == before {
                    return invalid(format!("no sequence is named '{name}'"));
                }
            }
            found.sort_unstable();
            found.dedup();
            return Ok(found);
        }
        if let Some(pattern) = &sel.matching {
            let re = regex(pattern)?;
            let found: Vec<usize> = (0..self.rows())
                .filter(|&row| re.is_match(self.name(row)))
                .collect();
            if found.is_empty() {
                return invalid(format!("no sequence name matches '{pattern}'"));
            }
            return Ok(found);
        }
        Ok((0..self.rows()).collect())
    }

    /// Column ranges checked against the current width.
    fn check_cols(&self, cols: &Ranges) -> Result<()> {
        if !self.is_aligned() {
            return invalid(NOT_ALIGNED);
        }
        if cols.end() > self.cols() {
            return invalid(format!(
                "column {} is past the last column ({})",
                cols.end(),
                self.cols()
            ));
        }
        Ok(())
    }

    /// Change the layer, remembering the state before for undo.
    fn edit(&mut self, name: &'static str, change: impl FnOnce(&mut Layer, &Source)) {
        let before = self.view.layer.clone();
        change(&mut self.view.layer, &self.view.source);
        self.view.layer.revision = self.next_revision;
        self.next_revision += 1;
        self.undo.push((before, name));
        if self.undo.len() > UNDO_LEVELS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn delete_rows(&mut self, sel: &RowSel) -> Result<Applied> {
        if sel.is_empty() {
            return invalid("say which sequences: --rows, --name or --match");
        }
        let rows = self.select_rows(sel)?;
        if rows.len() == self.rows() {
            return invalid("that would delete every sequence");
        }
        const NAMED: usize = 5;
        let mut note = format!(
            "removed: {}",
            rows.iter()
                .take(NAMED)
                .map(|&row| self.name(row))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if rows.len() > NAMED {
            note.push_str(&format!(" and {} more", rows.len() - NAMED));
        }
        let ranges = to_ranges(&rows);
        self.edit("delete-rows", |layer, source| {
            layer.delete_rows(source, &ranges)
        });
        Ok(Applied {
            message: format!(
                "deleted {}; now {}",
                count(rows.len(), "sequence"),
                self.shape()
            ),
            note: Some(note),
            created: None,
        })
    }

    fn delete_cols(&mut self, cols: &Ranges) -> Result<Applied> {
        self.check_cols(cols)?;
        if cols.count() == self.cols() {
            return invalid("that would delete every column");
        }
        self.edit("delete-cols", |layer, _| layer.delete_cols(cols));
        Ok(Applied::new(format!(
            "deleted {}; now {}",
            count(cols.count(), "column"),
            self.shape()
        )))
    }

    fn fill(&mut self, sel: &RowSel, cols: Option<&Ranges>, with: char) -> Result<Applied> {
        if sel.is_empty() && cols.is_none() {
            return invalid("say what to fill: --rows, --name, --match or --cols");
        }
        if !with.is_ascii_graphic() || with == '>' {
            return invalid(
                "the fill character must be one printable ASCII character other than '>'",
            );
        }
        if !self.is_aligned() {
            return invalid(NOT_ALIGNED);
        }
        let all_cols = Ranges::new(std::iter::once(0..self.cols()));
        let Some(cols) = cols.or(all_cols.as_ref()) else {
            return invalid("there are no columns to fill");
        };
        self.check_cols(cols)?;
        let rows = to_ranges(&self.select_rows(sel)?);
        self.edit("fill", |layer, _| layer.fill(&rows, cols, with as u8));
        Ok(Applied::new(format!(
            "filled {} x {} with '{with}'",
            count(rows.count(), "sequence"),
            count(cols.count(), "column")
        )))
    }

    fn overwrite(&mut self, row: usize, col: usize, seq: &str) -> Result<Applied> {
        if !self.is_aligned() {
            return invalid(NOT_ALIGNED);
        }
        if row == 0 || col == 0 {
            return invalid("positions start at 1");
        }
        if seq.is_empty() {
            return invalid("--seq is empty");
        }
        if !seq.bytes().all(|b| b.is_ascii_graphic() && b != b'>') {
            return invalid("--seq may hold only printable ASCII characters other than '>'");
        }
        if row > self.rows() {
            return invalid(format!("row {row} is past the last row ({})", self.rows()));
        }
        let last = col - 1 + seq.len();
        if last > self.cols() {
            return invalid(format!(
                "column {last} is past the last column ({})",
                self.cols()
            ));
        }
        self.edit("overwrite", |layer, _| {
            layer.overwrite(row - 1, col - 1, seq.as_bytes())
        });
        Ok(Applied::new(format!(
            "overwrote {} of row {row} from column {col}",
            count(seq.len(), "column")
        )))
    }

    fn rename_one(&mut self, sel: &RowSel, to: &str) -> Result<Applied> {
        let rows = self.select_rows(sel)?;
        let &[row] = rows.as_slice() else {
            return invalid(format!(
                "--to renames one sequence, but {} are selected",
                rows.len()
            ));
        };
        let to = to.trim();
        if to.is_empty() {
            return invalid(format!("a name cannot be empty (row {})", row + 1));
        }
        let was = self.name(row).to_string();
        self.edit("rename", |layer, _| layer.rename(row, to.to_string()));
        Ok(Applied {
            message: format!("renamed row {} to '{to}'", row + 1),
            note: Some(format!("was: {was}")),
            created: None,
        })
    }

    fn rename_matching(&mut self, sel: &RowSel, pattern: &str, replace: &str) -> Result<Applied> {
        let re = regex(pattern)?;
        check_replacement(&re, replace)?;
        let rows = self.select_rows(sel)?;
        let mut matched = false;
        let mut renames: Vec<(usize, String)> = Vec::new();
        for &row in &rows {
            let name = self.name(row);
            if !re.is_match(name) {
                continue;
            }
            matched = true;
            let renamed = re.replace_all(name, replace);
            let renamed = renamed.trim();
            if renamed.is_empty() {
                return invalid(format!("a name cannot be empty (row {})", row + 1));
            }
            if renamed != name {
                renames.push((row, renamed.to_string()));
            }
        }
        if !matched {
            return invalid(format!("no selected name contains a match for '{pattern}'"));
        }
        if renames.is_empty() {
            return invalid("that replacement leaves every name as it is");
        }
        let renamed_rows: Vec<usize> = renames.iter().map(|(row, _)| *row).collect();
        self.edit("rename", |layer, _| {
            for (row, name) in renames {
                layer.rename(row, name);
            }
        });

        let mut uses: HashMap<&str, usize> = HashMap::new();
        for row in 0..self.rows() {
            *uses.entry(self.name(row)).or_default() += 1;
        }
        let shared = renamed_rows
            .iter()
            .filter(|&&row| uses[self.name(row)] > 1)
            .count();
        let mut message = format!(
            "renamed {} of {}",
            renamed_rows.len(),
            count(rows.len(), "sequence")
        );
        if shared > 0 {
            message.push_str(&format!(
                "; {shared} of them now share a name with another sequence"
            ));
        }
        Ok(Applied::new(message))
    }
}

impl Alignment {
    /// Write to a new file. Changes nothing in the alignment or its history.
    fn extract(
        &self,
        sel: &RowSel,
        cols: Option<&Ranges>,
        wrap: Option<usize>,
        out: &Path,
    ) -> Result<Applied> {
        if wrap == Some(0) {
            return invalid("--wrap must be at least 1");
        }
        let rows = self.select_rows(sel)?;
        if let Some(cols) = cols {
            self.check_cols(cols)?;
        }
        let size = write::write_fasta(&self.view, &rows, cols, wrap, out)?;
        let sequences = count(rows.len(), "sequence");
        let shape = match cols {
            Some(cols) => format!("{sequences} x {}", count(cols.count(), "column")),
            None if self.is_aligned() => format!("{sequences} x {}", count(self.cols(), "column")),
            None => sequences,
        };
        // The full path, so the log says where the file went whatever the
        // working directory was.
        let full = std::fs::canonicalize(out).unwrap_or_else(|_| out.to_path_buf());
        Ok(Applied {
            message: format!("wrote {} ({shape})", out.display()),
            note: Some(format!("output {} size={size}", full.display())),
            created: Some(out.to_path_buf()),
        })
    }
}

/// Rows (ascending, at least one) as ranges.
fn to_ranges(rows: &[usize]) -> Ranges {
    Ranges::new(rows.iter().map(|&row| row..row + 1)).expect("a selection is never empty")
}

/// `1 sequence`, `3 sequences`.
pub(crate) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Nucleotide if at least 90% of the first 10,000 residues that are not gaps
/// are one of `ACGTUN`; with no residues to go by, nucleotide.
fn guess_alphabet(source: &Source) -> Alphabet {
    const SAMPLE: usize = 10_000;
    // Stop reading eventually even if the file starts with nothing but gaps.
    const MOST_BYTES: usize = 1_000_000;
    let (mut seen, mut nucleotide, mut read) = (0, 0, 0);
    let mut row = Vec::new();
    for id in 0..source.rows() {
        row.clear();
        source.read(id, 0..source.len(id).min(MOST_BYTES), &mut row);
        read += row.len();
        for &residue in row.iter().filter(|&&b| b != b'-' && b != b'.') {
            seen += 1;
            if b"ACGTUNacgtun".contains(&residue) {
                nucleotide += 1;
            }
            if seen == SAMPLE {
                break;
            }
        }
        if seen == SAMPLE || read >= MOST_BYTES {
            break;
        }
    }
    if nucleotide * 10 >= seen * 9 {
        Alphabet::Nucleotide
    } else {
        Alphabet::Protein
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::{Alignment, Applied};
    use crate::{Command, Result};

    /// Three sequences of ten columns, used throughout the tests.
    pub(crate) const TRIO: &str = ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n";

    /// Apply `msa <text>`.
    pub(crate) fn run(aln: &mut Alignment, text: &str) -> Result<Applied> {
        match Command::parse_line(&format!("msa {text}")).unwrap() {
            Command::Msa(cmd) => aln.apply(&cmd),
            other => panic!("not an alignment command: {other:?}"),
        }
    }

    /// Every row as (name, residues).
    pub(crate) fn grid(aln: &Alignment) -> Vec<(String, String)> {
        (0..aln.rows())
            .map(|row| {
                let mut out = Vec::new();
                aln.residues(row, 0..aln.cols(), &mut out);
                (aln.name(row).to_string(), String::from_utf8(out).unwrap())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{TRIO, grid, run};
    use super::*;

    fn text(aln: &Alignment, row: usize) -> String {
        grid(aln)[row].1.clone()
    }

    #[test]
    fn delete_rows_by_number_name_and_pattern() {
        let mut a = Alignment::from_text(TRIO);
        let done = run(&mut a, "delete-rows --rows 1-2").unwrap();
        assert_eq!(
            done.message,
            "deleted 2 sequences; now 1 sequence x 10 columns"
        );
        assert_eq!(done.note.as_deref(), Some("removed: s1, s2"));
        let mut a = Alignment::from_text(TRIO);
        run(&mut a, "delete-rows --name s2").unwrap();
        run(&mut a, "delete-rows --match third").unwrap();
        assert_eq!((a.rows(), a.name(0)), (1, "s1"));
    }

    #[test]
    fn long_removal_notes_are_shortened() {
        let text: String = (1..=8).map(|i| format!(">n{i}\nAC\n")).collect();
        let mut a = Alignment::from_text(&text);
        let done = run(&mut a, "delete-rows --rows 1-7").unwrap();
        assert_eq!(
            done.note.as_deref(),
            Some("removed: n1, n2, n3, n4, n5 and 2 more")
        );
    }

    #[test]
    fn duplicate_names() {
        let mut a = Alignment::from_text(">x\nAC\n>y\nAC\n>x\nGG\n");
        run(&mut a, "rename --rows 3 --to z").unwrap();
        assert_eq!([a.name(0), a.name(2)], ["x", "z"]);
        run(&mut a, "rename --rows 3 --to x").unwrap();
        run(&mut a, "delete-rows --name x").unwrap();
        assert_eq!((a.rows(), a.name(0)), (1, "y"));
    }

    #[test]
    fn delete_cols_and_fill() {
        let mut a = Alignment::from_text(TRIO);
        assert_eq!(
            run(&mut a, "delete-cols --cols 1-3").unwrap().message,
            "deleted 3 columns; now 3 sequences x 7 columns"
        );
        assert_eq!(
            run(&mut a, "fill --rows 1-2 --cols 2-4 --with N")
                .unwrap()
                .message,
            "filled 2 sequences x 3 columns with 'N'"
        );
        assert_eq!(text(&a, 0), "TNNNGTA");
        run(&mut a, "fill --cols 7 --with=-").unwrap();
        assert_eq!([text(&a, 0), text(&a, 2)], ["TNNNGT-", "-TACGT-"]);
        run(&mut a, "fill --name s2 --with x").unwrap();
        assert_eq!(text(&a, 1), "xxxxxxx");
    }

    #[test]
    fn rename_one_and_by_pattern() {
        let mut a = Alignment::from_text(TRIO);
        let done = run(&mut a, "rename --name 's3 third one' --to ' third '").unwrap();
        assert_eq!(
            (done.message.as_str(), done.note.as_deref()),
            ("renamed row 3 to 'third'", Some("was: s3 third one"))
        );
        assert_eq!(
            run(&mut a, "rename --regex '^s(\\d)$' --replace 'seq_$1'")
                .unwrap()
                .message,
            "renamed 2 of 3 sequences"
        );
        assert_eq!(
            [a.name(0), a.name(1), a.name(2)],
            ["seq_1", "seq_2", "third"]
        );
        assert_eq!(
            run(&mut a, "rename --rows 1-2 --regex '_\\d' --replace ''")
                .unwrap()
                .message,
            "renamed 2 of 2 sequences; 2 of them now share a name with another sequence"
        );
    }

    #[test]
    fn refusals_change_nothing() {
        let mut a = Alignment::from_text(TRIO);
        for (cmd, message) in [
            (
                "delete-rows",
                "say which sequences: --rows, --name or --match",
            ),
            ("delete-rows --rows 2-15", "row 15 is past the last row (3)"),
            (
                "delete-cols --cols 300",
                "column 300 is past the last column (10)",
            ),
            ("delete-rows --name X", "no sequence is named 'X'"),
            ("delete-rows --match zz", "no sequence name matches 'zz'"),
            ("delete-rows --match '('", "bad regular expression '('"),
            ("delete-rows --rows 1-3", "that would delete every sequence"),
            ("delete-cols --cols 1-10", "that would delete every column"),
            (
                "fill --with N",
                "say what to fill: --rows, --name, --match or --cols",
            ),
            (
                "fill --cols 1 --with '>'",
                "the fill character must be one printable ASCII character other than '>'",
            ),
            (
                "fill --cols 1 --with é",
                "the fill character must be one printable ASCII character other than '>'",
            ),
            (
                "fill --cols 11 --with N",
                "column 11 is past the last column (10)",
            ),
            (
                "rename --rows 1",
                "say the new name with --to, or use --regex with --replace",
            ),
            (
                "rename --to X",
                "--to renames one sequence, but 3 are selected",
            ),
            (
                "rename --rows 2 --to '  '",
                "a name cannot be empty (row 2)",
            ),
            (
                "rename --regex '^s2$' --replace ''",
                "a name cannot be empty (row 2)",
            ),
            (
                "rename --regex zz --replace y",
                "no selected name contains a match for 'zz'",
            ),
            (
                "rename --regex s --replace s",
                "that replacement leaves every name as it is",
            ),
            (
                "rename --regex '(' --replace y",
                "bad regular expression '('",
            ),
        ] {
            assert_eq!(run(&mut a, cmd).unwrap_err().to_string(), message, "{cmd}");
            assert_eq!(grid(&a), grid(&Alignment::from_text(TRIO)), "{cmd}");
        }
        assert_eq!(a.undo().unwrap_err().to_string(), "nothing to undo");
        let mut ragged = Alignment::from_text(">a\nACGT\n>b\nAC\n");
        for cmd in [
            "delete-cols --cols 1",
            "fill --cols 1 --with N",
            "fill --rows 1 --with N",
        ] {
            assert_eq!(
                run(&mut ragged, cmd).unwrap_err().to_string(),
                "sequences differ in length, so columns cannot be selected"
            );
        }
        run(&mut ragged, "rename --rows 2 --to c").unwrap(); // names and rows still work
        run(&mut ragged, "delete-rows --rows 1").unwrap();
    }

    #[test]
    fn undo_and_redo() {
        let mut a = Alignment::from_text(TRIO);
        run(&mut a, "delete-cols --cols 1-3").unwrap();
        run(&mut a, "rename --rows 1 --to one").unwrap();
        assert_eq!(
            a.undo().unwrap().message,
            "undid rename; now 3 sequences x 7 columns"
        );
        assert_eq!(
            a.undo().unwrap().message,
            "undid delete-cols; now 3 sequences x 10 columns"
        );
        assert_eq!(grid(&a), grid(&Alignment::from_text(TRIO)));
        assert_eq!(
            a.redo().unwrap().message,
            "redid delete-cols; now 3 sequences x 7 columns"
        );
        run(&mut a, "fill --cols 1 --with N").unwrap(); // a new edit clears redo
        assert_eq!(a.redo().unwrap_err().to_string(), "nothing to redo");
    }

    #[test]
    fn undo_keeps_twenty_levels() {
        let mut a = Alignment::from_text(&format!(">a\n{}\n", "A".repeat(30)));
        for _ in 0..21 {
            run(&mut a, "delete-cols --cols 1").unwrap();
        }
        for _ in 0..20 {
            a.undo().unwrap();
        }
        assert_eq!(
            (a.cols(), a.undo().unwrap_err().to_string()),
            (29, "nothing to undo".to_string())
        );
    }

    #[test]
    fn a_snapshot_can_be_put_back() {
        let mut a = Alignment::from_text(TRIO);
        let before = a.snapshot();
        run(&mut a, "delete-cols --cols 1-3").unwrap();
        a.restore(before);
        assert_eq!(grid(&a), grid(&Alignment::from_text(TRIO)));
    }

    #[test]
    fn a_replacement_may_only_name_groups_the_pattern_has() {
        let mut a = Alignment::from_text(">Homo_sapiens\nAC\n>Mus_musculus\nAC\n");
        let swap = r"rename --regex '^(\w+?)_(\w+)$' --replace";
        assert_eq!(
            run(&mut a, &format!("{swap} '$2_$1'"))
                .unwrap_err()
                .to_string(),
            "--replace refers to group '2_', which the pattern does not have; \
             write ${2}_ for group 2 followed by '_'"
        );
        assert_eq!(
            run(&mut a, &format!("{swap} '$3'"))
                .unwrap_err()
                .to_string(),
            "--replace refers to group '3', which the pattern does not have"
        );
        assert_eq!(
            run(&mut a, &format!("{swap} 'x${{genus}}'"))
                .unwrap_err()
                .to_string(),
            "--replace refers to group 'genus', which the pattern does not have"
        );
        assert_eq!(a.name(0), "Homo_sapiens");

        run(&mut a, &format!("{swap} '${{2}}_$1'")).unwrap();
        assert_eq!([a.name(0), a.name(1)], ["sapiens_Homo", "musculus_Mus"]);
        run(
            &mut a,
            r"rename --regex '(?<first>\w+)_(\w+)' --replace '$2 $$ ${first}'",
        )
        .unwrap();
        assert_eq!(a.name(0), "Homo $ sapiens");
        run(&mut a, r"rename --regex ' \$ ' --replace '$0$'").unwrap();
        assert_eq!(a.name(0), "Homo $ $sapiens");
    }

    fn seqs(aln: &Alignment) -> Vec<String> {
        grid(aln).into_iter().map(|(_, seq)| seq).collect()
    }

    #[test]
    fn overwrite_replaces_a_run_in_one_row() {
        let mut aln = Alignment::from_text(TRIO);
        let done = run(&mut aln, "overwrite --row 2 --col 4 --seq=-NNa").unwrap();
        assert_eq!(done.message, "overwrote 4 columns of row 2 from column 4");
        assert_eq!(seqs(&aln), ["ACGT-ACGTA", "ACG-NNaG-A", "AC--TACGTA"]);
    }

    #[test]
    fn overwrite_is_one_undo_step() {
        let mut aln = Alignment::from_text(TRIO);
        run(&mut aln, "overwrite --row 1 --col 1 --seq NNNXX").unwrap();
        assert_eq!(
            aln.undo().unwrap().message,
            "undid overwrite; now 3 sequences x 10 columns"
        );
        assert_eq!(seqs(&aln)[0], "ACGT-ACGTA");
        assert!(aln.undo().is_err());
    }

    #[test]
    fn overwrite_is_tied_to_the_columns_it_was_typed_into() {
        // Typed after a deletion, or before it: the same cells end up changed.
        let mut after = Alignment::from_text(TRIO);
        run(&mut after, "delete-cols --cols 2-3").unwrap();
        run(&mut after, "overwrite --row 1 --col 2 --seq XY").unwrap();
        let mut before = Alignment::from_text(TRIO);
        run(&mut before, "overwrite --row 1 --col 4 --seq XY").unwrap();
        run(&mut before, "delete-cols --cols 2-3").unwrap();
        assert_eq!(seqs(&after)[0], "AXYACGTA");
        assert_eq!(seqs(&after), seqs(&before));
    }

    #[test]
    fn overwrite_refusals_change_nothing() {
        const BAD_SEQ: &str = "--seq may hold only printable ASCII characters other than '>'";
        for (text, message) in [
            (
                "overwrite --row 4 --col 1 --seq A",
                "row 4 is past the last row (3)",
            ),
            (
                "overwrite --row 1 --col 9 --seq ACG",
                "column 11 is past the last column (10)",
            ),
            ("overwrite --row 0 --col 1 --seq A", "positions start at 1"),
            ("overwrite --row 1 --col 0 --seq A", "positions start at 1"),
            ("overwrite --row 1 --col 1 --seq ''", "--seq is empty"),
            ("overwrite --row 1 --col 1 --seq 'A>C'", BAD_SEQ),
            ("overwrite --row 1 --col 1 --seq 'A C'", BAD_SEQ),
        ] {
            let mut aln = Alignment::from_text(TRIO);
            assert_eq!(
                run(&mut aln, text).unwrap_err().to_string(),
                message,
                "{text}"
            );
            assert_eq!(seqs(&aln)[0], "ACGT-ACGTA");
            assert!(aln.undo().is_err(), "{text}");
        }
        let mut ragged = Alignment::from_text(">a\nAC\n>b\nACGT\n");
        assert_eq!(
            run(&mut ragged, "overwrite --row 1 --col 1 --seq A")
                .unwrap_err()
                .to_string(),
            "sequences differ in length, so columns cannot be selected"
        );
    }

    #[test]
    fn a_view_can_be_read_from_another_thread() {
        let aln = Alignment::from_text(TRIO);
        let view = aln.view().clone();
        let read = std::thread::spawn(move || {
            let mut out = Vec::new();
            view.residues(2, 0..4, &mut out);
            (view.name(2).to_string(), out)
        });
        assert_eq!(
            read.join().unwrap(),
            ("s3 third one".to_string(), b"AC--".to_vec())
        );
    }

    #[test]
    fn a_view_keeps_the_state_it_was_taken_from() {
        let mut aln = Alignment::from_text(TRIO);
        let before = aln.view().clone();
        run(&mut aln, "delete-rows --rows 1").unwrap();
        assert_eq!((before.rows(), aln.rows()), (3, 2));
        assert_eq!(before.name(0), "s1");
    }

    #[test]
    fn revisions_change_with_edits_and_come_back_with_undo() {
        let mut aln = Alignment::from_text(TRIO);
        assert_eq!(aln.revision(), 0);
        run(&mut aln, "fill --cols 1 --with N").unwrap();
        let first = aln.revision();
        assert_ne!(first, 0);
        aln.undo().unwrap();
        assert_eq!(aln.revision(), 0);
        aln.redo().unwrap();
        assert_eq!((aln.revision(), aln.view().revision()), (first, first));
        aln.undo().unwrap();
        run(&mut aln, "fill --cols 2 --with N").unwrap();
        assert!(
            ![0, first].contains(&aln.revision()),
            "a new state gets a number never used"
        );
    }

    #[test]
    fn find_names_ignores_case_and_reports_bad_patterns() {
        let aln = Alignment::from_text(TRIO);
        assert_eq!(aln.find_names("S[12]").unwrap(), [0, 1]);
        assert_eq!(aln.find_names("third").unwrap(), [2]);
        assert!(aln.find_names("zzz").unwrap().is_empty());
        assert_eq!(
            aln.find_names("(").unwrap_err().to_string(),
            "bad regular expression '('"
        );
    }
}
