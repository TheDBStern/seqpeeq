//! Alignments: a FASTA file read in place, under a layer of edits.

pub(crate) mod layer;
pub(crate) mod source;

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use crate::Result;
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
    source: Arc<Source>,
    layer: Layer,
    alphabet: Alphabet,
}

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
            layer: Layer::new(&source),
            source: Arc::new(source),
            alphabet,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_text(text: &str) -> Alignment {
        Alignment::new(Source::from_vec(text.into(), Path::new("test.fa")).unwrap())
    }

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

    pub fn alphabet(&self) -> Alphabet {
        self.alphabet
    }

    pub fn name(&self, row: usize) -> &str {
        self.layer.name(&self.source, row)
    }

    /// The length of one row; the same as [`Alignment::cols`] when aligned.
    pub fn row_len(&self, row: usize) -> usize {
        self.layer.row_len(&self.source, row)
    }

    /// Append the residues of `row` in columns `cols` to `out`. Columns past
    /// the end of the row add nothing.
    pub fn residues(&self, row: usize, cols: Range<usize>, out: &mut Vec<u8>) {
        self.layer.read(&self.source, row, cols, out);
    }

    /// The size in words, such as `3 sequences x 10 columns`.
    pub fn shape(&self) -> String {
        let sequences = count(self.rows(), "sequence");
        if self.is_aligned() {
            format!("{sequences} x {}", count(self.cols(), "column"))
        } else {
            format!(
                "{sequences}, not aligned (lengths {}-{})",
                self.layer.min_len(),
                self.cols()
            )
        }
    }
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
    use super::Alignment;

    /// Three sequences of ten columns, used throughout the tests.
    pub(crate) const TRIO: &str = ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n";

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
