//! A reference FASTA attached to a BAM, read a stretch at a time.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use noodles::{bgzf, fasta};

use crate::bam::index::file_name;
use crate::progress::Progress;
use crate::{Error, Result};

/// A reference FASTA and its index: plain (its `.fai` read, or built in
/// memory) or bgzipped (with its `.fai` and `.gzi`).
#[derive(Clone)]
pub struct Reference {
    path: PathBuf,
    index: Arc<fasta::fai::Index>,
    /// Each sequence's place in `index`, by name.
    names: Arc<HashMap<String, usize>>,
    /// One open reader, kept between fetches.
    reader: Arc<Mutex<Box<dyn ReadSeek>>>,
}

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

impl std::fmt::Debug for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reference")
            .field("path", &self.path)
            .field("sequences", &self.names.len())
            .finish()
    }
}

/// How a file is compressed, from its first bytes.
enum Packing {
    Plain,
    Gzip,
    Bgzip,
}

fn packing(path: &Path) -> io::Result<Packing> {
    let mut head = [0u8; 18];
    let mut file = File::open(path)?;
    let mut read = 0;
    while read < head.len() {
        let n = file.read(&mut head[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    if read < 2 || head[..2] != [0x1f, 0x8b] {
        return Ok(Packing::Plain);
    }
    // BGZF: a gzip member whose extra field (FLG.FEXTRA) holds a `BC`
    // subfield.
    let bgzip = read >= 14 && head[3] & 0x04 != 0 && head[12..14] == *b"BC";
    Ok(if bgzip { Packing::Bgzip } else { Packing::Gzip })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Why a plain FASTA could not be indexed.
enum Problem {
    Io(io::Error),
    Cancelled,
    /// What is wrong with the file, for a person to read.
    Bad(String),
}

impl From<io::Error> for Problem {
    fn from(cause: io::Error) -> Problem {
        Problem::Io(cause)
    }
}

const REWRAP: &str = "Rewrap the file, for example with seqkit seq -w 60";

/// A sequence being indexed.
struct Open {
    name: String,
    /// Where its first base is.
    position: u64,
    length: u64,
    /// Bases, and bytes with the line end, of its first line; zero before
    /// that line is read.
    line_bases: u64,
    line_width: u64,
    /// The last line read, when it may only be the sequence's last: one
    /// shorter or ending differently from the first, or a blank one.
    last: Option<(u64, Last)>,
}

enum Last {
    Short(u64),
    Ending,
    Blank,
}

impl Open {
    /// Its index record; none for a sequence with no bases, which
    /// `samtools faidx` leaves out too.
    fn record(self) -> Option<fasta::fai::Record> {
        let line_bases = std::num::NonZero::new(self.line_bases)?;
        let line_width = std::num::NonZero::new(self.line_width)?;
        Some(fasta::fai::Record::new(
            self.name,
            self.length,
            self.position,
            line_bases,
            line_width,
        ))
    }
}

/// One line of a FASTA, without its bases.
enum Line {
    /// A `>` line, whose text is left in the header buffer.
    Header,
    /// Any other line: how many bytes it has up to its last that is not
    /// whitespace (none for a blank line), and whether whitespace comes
    /// before that.
    Bases { bases: u64, spaced: bool },
}

/// Read the next line and its width with the line end; none at the end of
/// the file. Only a header is kept: a sequence line, which may be a whole
/// chromosome, is counted as it streams past.
fn read_line(
    reader: &mut impl io::BufRead,
    header: &mut Vec<u8>,
    progress: &Progress,
) -> std::result::Result<Option<(Line, u64)>, Problem> {
    match reader.fill_buf()?.first() {
        None => return Ok(None),
        Some(b'>') => {
            header.clear();
            let width = reader.read_until(b'\n', header)? as u64;
            return Ok(Some((Line::Header, width)));
        }
        Some(_) => {}
    }
    let (mut width, mut bases, mut first_space) = (0u64, 0u64, None);
    loop {
        if progress.is_cancelled() {
            return Err(Problem::Cancelled);
        }
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let (text, ends) = match memchr::memchr(b'\n', buf) {
            Some(i) => (&buf[..i], true),
            None => (buf, false),
        };
        if let Some(i) = text.iter().rposition(|b| !b.is_ascii_whitespace()) {
            bases = width + i as u64 + 1;
        }
        if first_space.is_none()
            && let Some(i) = text.iter().position(u8::is_ascii_whitespace)
        {
            first_space = Some(width + i as u64);
        }
        let used = text.len() + usize::from(ends);
        reader.consume(used);
        width += used as u64;
        if ends {
            break;
        }
    }
    let spaced = first_space.is_some_and(|at| at < bases);
    Ok(Some((Line::Bases { bases, spaced }, width)))
}

/// Index a plain FASTA as `samtools faidx` does: every line of a sequence
/// but its last holds the same number of bases, and whitespace ending a
/// line is not a base. It is a little more lenient: blank lines may come
/// before a sequence's first bases or after its last (as when genomes are
/// pasted into one file), and its last line may end differently, since
/// reading a stretch never goes past a sequence's length.
fn index(
    mut reader: impl io::BufRead,
    progress: &Progress,
) -> std::result::Result<Vec<fasta::fai::Record>, Problem> {
    let mut records = Vec::new();
    let mut open: Option<Open> = None;
    let mut header = Vec::new();
    let (mut at, mut number) = (0u64, 0u64);
    while let Some((line, width)) = read_line(&mut reader, &mut header, progress)? {
        at += width;
        number += 1;
        let (bases, spaced) = match line {
            Line::Bases { bases, spaced } => (bases, spaced),
            Line::Header => {
                records.extend(open.take().and_then(Open::record));
                let text = String::from_utf8_lossy(&header[1..]);
                let Some(name) = text.split_ascii_whitespace().next() else {
                    return Err(Problem::Bad(format!(
                        "the header on line {number} has no name"
                    )));
                };
                open = Some(Open {
                    name: name.to_string(),
                    position: at,
                    length: 0,
                    line_bases: 0,
                    line_width: 0,
                    last: None,
                });
                continue;
            }
        };
        let Some(seq) = open.as_mut() else {
            if bases == 0 {
                continue;
            }
            return Err(Problem::Bad(format!(
                "line {number} comes before the first > header"
            )));
        };
        let name = &seq.name;
        if bases == 0 {
            if seq.line_bases == 0 {
                // Before the first bases: they start after it.
                seq.position = at;
            } else if seq.last.is_none() {
                seq.last = Some((number, Last::Blank));
            }
            continue;
        }
        if spaced {
            return Err(Problem::Bad(format!(
                "line {number} (in {name}) has a space or tab among its bases. {REWRAP}"
            )));
        }
        match seq.last {
            None => {}
            Some((line, Last::Blank)) => {
                return Err(Problem::Bad(format!(
                    "line {line} (in {name}) is blank, but more of {name} follows it. {REWRAP}"
                )));
            }
            Some((line, Last::Short(bases))) => {
                return Err(wrong_length(line, name, bases, seq.line_bases));
            }
            Some((line, Last::Ending)) => {
                return Err(Problem::Bad(format!(
                    "line {line} (in {name}) ends differently from {name}'s first line, in its \
                     line end or trailing spaces. {REWRAP}"
                )));
            }
        }
        seq.length += bases;
        if seq.line_bases == 0 {
            seq.line_bases = bases;
            seq.line_width = width;
            continue;
        }
        if bases > seq.line_bases {
            return Err(wrong_length(number, name, bases, seq.line_bases));
        } else if bases < seq.line_bases {
            seq.last = Some((number, Last::Short(bases)));
        } else if width != seq.line_width {
            // Another line end or trailing spaces, or none at the end of
            // the file.
            seq.last = Some((number, Last::Ending));
        }
    }
    records.extend(open.and_then(Open::record));
    Ok(records)
}

/// A line inside a sequence with more or fewer bases than its first.
fn wrong_length(line: u64, name: &str, bases: u64, first: u64) -> Problem {
    Problem::Bad(format!(
        "line {line} (in {name}) has {bases} bases, but {name}'s first line has {first}; \
         only a sequence's last line may be shorter. {REWRAP}"
    ))
}

impl Reference {
    /// Open `path`, checking that a stretch of it can be read.
    pub(crate) fn open(path: &Path, progress: &Progress) -> Result<Reference> {
        let failed = |cause: io::Error| Error::Io {
            path: path.to_path_buf(),
            cause,
        };
        let name = file_name(path);
        let fai = with_suffix(path, ".fai");
        let packed = packing(path).map_err(failed)?;
        let index = match packed {
            Packing::Gzip => {
                return Err(Error::Invalid(format!(
                    "{name} is gzip, not bgzip: reading a stretch needs bgzip -i"
                )));
            }
            Packing::Bgzip => {
                if !fai.exists() || !with_suffix(path, ".gzi").exists() {
                    return Err(Error::Invalid(format!(
                        "{name} needs {name}.fai and {name}.gzi (samtools faidx makes both)"
                    )));
                }
                fasta::fai::fs::read(&fai).map_err(|cause| Error::Io { path: fai, cause })?
            }
            Packing::Plain if fai.exists() => {
                fasta::fai::fs::read(&fai).map_err(|cause| Error::Io { path: fai, cause })?
            }
            Packing::Plain => {
                let file = File::open(path).map_err(failed)?;
                let records = index(BufReader::with_capacity(1 << 16, file), progress).map_err(
                    |problem| match problem {
                        Problem::Io(cause) => failed(cause),
                        Problem::Cancelled => Error::Cancelled,
                        Problem::Bad(message) => Error::Format {
                            path: path.to_path_buf(),
                            message: format!("not a readable FASTA: {message}"),
                        },
                    },
                )?;
                fasta::fai::Index::from(records)
            }
        };
        let file = File::open(path).map_err(failed)?;
        let reader: Box<dyn ReadSeek> = match packed {
            Packing::Bgzip => {
                let gzi = with_suffix(path, ".gzi");
                let gzi =
                    bgzf::gzi::fs::read(&gzi).map_err(|cause| Error::Io { path: gzi, cause })?;
                Box::new(bgzf::io::IndexedReader::new(file, gzi))
            }
            _ => Box::new(BufReader::new(file)),
        };
        let names = index
            .as_ref()
            .iter()
            .enumerate()
            .map(|(i, record)| (record.name().to_string(), i))
            .collect();
        Ok(Reference {
            path: path.to_path_buf(),
            index: Arc::new(index),
            names: Arc::new(names),
            reader: Arc::new(Mutex::new(reader)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the FASTA has a sequence called `name`.
    pub fn has(&self, name: &str) -> bool {
        self.names.contains_key(name)
    }

    /// The bases of `name` from `start` to `end`, 1-based and inclusive
    /// (cut to the sequence's end).
    pub fn fetch(&self, name: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        let failed = |cause: io::Error| Error::Io {
            path: self.path.clone(),
            cause,
        };
        let Some(&i) = self.names.get(name) else {
            return Err(Error::Invalid(format!(
                "{name} is not in {}",
                file_name(&self.path)
            )));
        };
        let records: &[fasta::fai::Record] = (*self.index).as_ref();
        let record = &records[i];
        let end = end.min(record.length());
        if start == 0 || start > end {
            return Ok(Vec::new());
        }
        let (line_bases, line_width) = (record.line_base_count().get(), record.line_width().get());
        let skip = start - 1;
        let at = record.position() + skip / line_bases * line_width + skip % line_bases;
        let want = (end - start + 1) as usize;
        let mut bases = Vec::with_capacity(want);
        let mut reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        reader.seek(SeekFrom::Start(at)).map_err(failed)?;
        let mut chunk = vec![0u8; (want + want / 50 + 64).min(1 << 20)];
        while bases.len() < want {
            let n = reader.read(&mut chunk).map_err(failed)?;
            if n == 0 {
                break;
            }
            for &b in &chunk[..n] {
                // Line ends, and any whitespace ending a line, are not
                // bases.
                if !b.is_ascii_whitespace() {
                    bases.push(b);
                    if bases.len() == want {
                        break;
                    }
                }
            }
        }
        Ok(bases)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open `text` as a reference, or say why not (without the path).
    fn open(text: &str) -> std::result::Result<Reference, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ref.fa");
        std::fs::write(&path, text).unwrap();
        Reference::open(&path, &Progress::new()).map_err(|e| {
            let prefix = format!("{}: ", path.display());
            e.to_string().trim_start_matches(&prefix).to_string()
        })
    }

    #[test]
    fn blank_lines_between_and_after_sequences_are_allowed() {
        // As when genomes are pasted into one file: issue #16.
        let r = open("\n>a one\nACGT\nACGT\nAC\n\n\n>b\nTTTT\nGG\n  \n>c\nCCC\n\n").unwrap();
        assert_eq!(r.fetch("a", 1, 10).unwrap(), b"ACGTACGTAC");
        assert_eq!(r.fetch("a", 4, 9).unwrap(), b"TACGTA");
        assert_eq!(r.fetch("a", 9, 99).unwrap(), b"AC", "cut to the end");
        assert_eq!(r.fetch("b", 3, 6).unwrap(), b"TTGG");
        assert_eq!(r.fetch("c", 1, 9).unwrap(), b"CCC");
    }

    #[test]
    fn a_clean_file_indexes_as_noodles_indexes_it() {
        let text = ">a desc\nACGTA\nCGTAC\nGT\n>b\nTTTTT\n>c\r\nAC\r\nGT\r\nA\r\n>d\nACGTA\nCGTAC";
        let ours = index(text.as_bytes(), &Progress::new()).ok().unwrap();
        let mut indexer = fasta::io::Indexer::new(text.as_bytes());
        let mut theirs = Vec::new();
        while let Some(record) = indexer.index_record().unwrap() {
            theirs.push(record);
        }
        assert_eq!(ours, theirs);
    }

    #[test]
    fn windows_line_ends_and_no_final_newline_are_allowed() {
        let r = open(">a\r\nACG\r\nTAC\r\nG\r\n\r\n>b\r\nTTT\r\nT").unwrap();
        assert_eq!(r.fetch("a", 2, 7).unwrap(), b"CGTACG");
        assert_eq!(r.fetch("b", 1, 4).unwrap(), b"TTTT");
    }

    #[test]
    fn a_short_line_inside_a_sequence_is_refused_with_its_line() {
        assert_eq!(
            open(">a\nACGT\n>b\nACGT\nAC\nACGT\n").unwrap_err(),
            "not a readable FASTA: line 5 (in b) has 2 bases, but b's first line has 4; \
             only a sequence's last line may be shorter. Rewrap the file, for example with \
             seqkit seq -w 60"
        );
        assert_eq!(
            open(">a\nACGT\nACGTA\n").unwrap_err(),
            "not a readable FASTA: line 3 (in a) has 5 bases, but a's first line has 4; \
             only a sequence's last line may be shorter. Rewrap the file, for example with \
             seqkit seq -w 60"
        );
    }

    #[test]
    fn a_blank_line_inside_a_sequence_is_refused_with_its_line() {
        assert_eq!(
            open(">a\nACGT\n\nACGT\n").unwrap_err(),
            "not a readable FASTA: line 3 (in a) is blank, but more of a follows it. \
             Rewrap the file, for example with seqkit seq -w 60"
        );
    }

    #[test]
    fn mixed_line_ends_inside_a_sequence_are_refused() {
        assert_eq!(
            open(">a\nACGT\r\nACGT\nAC\n").unwrap_err(),
            "not a readable FASTA: line 3 (in a) ends differently from a's first line, in its \
             line end or trailing spaces. Rewrap the file, for example with seqkit seq -w 60"
        );
    }

    #[test]
    fn blank_lines_before_a_sequences_first_bases_are_allowed() {
        let r = open(">a\n\nACGT\nAC\n>b\n\r\n \t\nGG\n").unwrap();
        assert_eq!(r.fetch("a", 1, 9).unwrap(), b"ACGTAC");
        assert_eq!(r.fetch("b", 1, 9).unwrap(), b"GG");
    }

    #[test]
    fn trailing_spaces_are_not_bases() {
        // As samtools faidx reads them.
        let r = open(">a\nACGT \nACGT \nAC\n").unwrap();
        assert_eq!(r.fetch("a", 1, 99).unwrap(), b"ACGTACGTAC");
        assert_eq!(r.fetch("a", 4, 6).unwrap(), b"TAC");
    }

    #[test]
    fn a_fai_for_trailing_spaces_reads_without_them() {
        // The .fai samtools faidx writes for this file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ref.fa");
        std::fs::write(&path, ">a\nACGT \nACGT \nAC\n").unwrap();
        std::fs::write(dir.path().join("ref.fa.fai"), "a\t10\t3\t4\t6\n").unwrap();
        let r = Reference::open(&path, &Progress::new()).unwrap();
        assert_eq!(r.fetch("a", 1, 99).unwrap(), b"ACGTACGTAC");
    }

    #[test]
    fn a_space_among_bases_is_refused() {
        assert_eq!(
            open(">a\nAC GT\nACGT\n").unwrap_err(),
            "not a readable FASTA: line 2 (in a) has a space or tab among its bases. \
             Rewrap the file, for example with seqkit seq -w 60"
        );
        assert_eq!(
            open(">a\nACGT\n \t\nACGT\n").unwrap_err(),
            "not a readable FASTA: line 3 (in a) is blank, but more of a follows it. \
             Rewrap the file, for example with seqkit seq -w 60"
        );
    }

    #[test]
    fn a_sequence_with_no_bases_is_left_out() {
        // As samtools faidx leaves it out of its .fai.
        let r = open(">a\n>b\nTTTT\n>c\n\n").unwrap();
        assert!(!r.has("a") && !r.has("c"));
        assert_eq!(r.fetch("b", 1, 4).unwrap(), b"TTTT");
    }

    #[test]
    fn indexing_stops_when_cancelled() {
        let progress = Progress::new();
        progress.cancel();
        let text = ">a\nACGT\n".repeat(10);
        assert!(matches!(
            index(text.as_bytes(), &progress),
            Err(Problem::Cancelled)
        ));
    }

    #[test]
    fn other_malformed_files_are_refused() {
        assert_eq!(
            open("ACGT\n>a\nACGT\n").unwrap_err(),
            "not a readable FASTA: line 1 comes before the first > header"
        );
        assert_eq!(
            open(">\nACGT\n").unwrap_err(),
            "not a readable FASTA: the header on line 1 has no name"
        );
    }
}
