//! The bytes of a FASTA file and where each record sits in them.
//!
//! Loading makes one pass to index the records. A record whose sequence
//! lines are regular is afterwards read straight out of the mapped file; only
//! untidy records are copied.

use std::fs::File;
use std::io::{self, Read};
use std::ops::{Deref, Range};
use std::path::Path;

use flate2::read::MultiGzDecoder;
use memchr::{memchr, memchr3};
use memmap2::Mmap;

use crate::{Error, Result};

/// The file's contents: mapped from disk, or held in memory when the file
/// had to be decompressed.
enum Bytes {
    Mapped(Mmap),
    Memory(Vec<u8>),
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(map) => map,
            Bytes::Memory(vec) => vec,
        }
    }
}

struct Record {
    /// Where the name is in the file.
    name: Range<usize>,
    seq: Seq,
}

enum Seq {
    /// Read in place: `len` residues from `start`, in lines of `line_bases`
    /// residues that each take `line_bytes` bytes with their line ending.
    InPlace {
        start: usize,
        len: usize,
        line_bases: usize,
        line_bytes: usize,
    },
    /// Copied at load with whitespace removed; an index into `Source::copied`.
    Copied(usize),
}

pub(crate) struct Source {
    bytes: Bytes,
    records: Vec<Record>,
    copied: Vec<Vec<u8>>,
    width: usize,
    min_len: usize,
}

// Not derived: that would print the whole file.
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("rows", &self.rows())
            .field("width", &self.width)
            .finish_non_exhaustive()
    }
}

impl Source {
    pub(crate) fn load(path: &Path) -> Result<Source> {
        let io = |cause| Error::Io {
            path: path.to_path_buf(),
            cause,
        };
        let file = File::open(path).map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        if meta.is_dir() {
            return Err(io(io::ErrorKind::IsADirectory.into()));
        }
        if meta.len() == 0 {
            // An empty file cannot be mapped on every platform.
            return Source::from_bytes(Bytes::Memory(Vec::new()), path);
        }
        // SAFETY: the mapping is only read. If another program truncates or
        // rewrites the file while it is open, reads may see changed data or
        // the process may be stopped by the system; seqpeeq itself never
        // writes to an input file.
        let map = unsafe { Mmap::map(&file) }.map_err(io)?;
        Source::from_bytes(Bytes::Mapped(map), path)
    }

    /// As [`Source::load`] for contents already in memory. `path` is used
    /// only in error messages.
    #[cfg(test)]
    pub(crate) fn from_vec(bytes: Vec<u8>, path: &Path) -> Result<Source> {
        Source::from_bytes(Bytes::Memory(bytes), path)
    }

    fn from_bytes(bytes: Bytes, path: &Path) -> Result<Source> {
        // Gzip is recognised by its first two bytes, not by the file name.
        // bgzip writes many gzip members one after another, so all are read.
        let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut plain = Vec::new();
            MultiGzDecoder::new(&bytes[..])
                .read_to_end(&mut plain)
                .map_err(|cause| Error::Io {
                    path: path.to_path_buf(),
                    cause,
                })?;
            Bytes::Memory(plain)
        } else {
            bytes
        };
        let (records, copied) = index(&bytes).map_err(|message| Error::Format {
            path: path.to_path_buf(),
            message,
        })?;
        let mut source = Source {
            bytes,
            records,
            copied,
            width: 0,
            min_len: 0,
        };
        let lengths = || (0..source.rows()).map(|id| source.len(id));
        let (width, min_len) = (lengths().max().unwrap_or(0), lengths().min().unwrap_or(0));
        source.width = width;
        source.min_len = min_len;
        Ok(source)
    }

    /// How many records the file holds.
    pub(crate) fn rows(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn name(&self, id: usize) -> &str {
        let range = self.records[id].name.clone();
        std::str::from_utf8(&self.bytes[range]).expect("names are checked when the file is loaded")
    }

    /// How many residues record `id` has.
    pub(crate) fn len(&self, id: usize) -> usize {
        match &self.records[id].seq {
            Seq::InPlace { len, .. } => *len,
            Seq::Copied(index) => self.copied[*index].len(),
        }
    }

    /// The length of the longest record.
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// The length of the shortest record.
    pub(crate) fn min_len(&self) -> usize {
        self.min_len
    }

    /// Append residues `cols` of record `id` to `out`. The part of `cols`
    /// past the end of the record adds nothing.
    pub(crate) fn read(&self, id: usize, cols: Range<usize>, out: &mut Vec<u8>) {
        let end = cols.end.min(self.len(id));
        if cols.start >= end {
            return;
        }
        match &self.records[id].seq {
            Seq::Copied(index) => out.extend_from_slice(&self.copied[*index][cols.start..end]),
            Seq::InPlace {
                start,
                line_bases,
                line_bytes,
                ..
            } => {
                let mut col = cols.start;
                while col < end {
                    let within = col % line_bases;
                    let take = (line_bases - within).min(end - col);
                    let from = start + (col / line_bases) * line_bytes + within;
                    out.extend_from_slice(&self.bytes[from..from + take]);
                    col += take;
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_mapped(&self, id: usize) -> bool {
        matches!(self.records[id].seq, Seq::InPlace { .. })
    }
}

/// The end of the line starting at `from`: where its content stops and where
/// the next line starts.
fn line_end(data: &[u8], from: usize) -> (usize, usize) {
    match memchr(b'\n', &data[from..]) {
        Some(offset) => (from + offset, from + offset + 1),
        None => (data.len(), data.len()),
    }
}

/// Find every record. Errors are messages for [`Error::Format`].
fn index(data: &[u8]) -> std::result::Result<(Vec<Record>, Vec<Vec<u8>>), String> {
    // Skip blank lines before the first record.
    let mut pos = 0;
    let mut line_number = 1;
    loop {
        if pos >= data.len() {
            return Err("no sequences found".to_string());
        }
        let (end, next) = line_end(data, pos);
        if !data[pos..end].iter().all(u8::is_ascii_whitespace) {
            break;
        }
        pos = next;
        line_number += 1;
    }
    if data[pos] != b'>' {
        return Err(format!(
            "line {line_number} does not start with '>': not a FASTA file"
        ));
    }

    let mut records = Vec::new();
    let mut copied = Vec::new();
    while pos < data.len() {
        // `pos` is at the `>` of a header line.
        let (header_end, seq_start) = line_end(data, pos);
        let name_start = pos + 1;
        let name = &data[name_start..header_end];
        let name_len = name.len()
            - name
                .iter()
                .rev()
                .take_while(|b| b.is_ascii_whitespace())
                .count();
        if std::str::from_utf8(&name[..name_len]).is_err() {
            return Err(format!(
                "sequence {} has a name that is not valid UTF-8",
                records.len() + 1
            ));
        }

        // Walk the sequence lines, checking whether they are regular enough
        // to be read in place later.
        let mut regular = true;
        let mut len = 0;
        let mut first: Option<usize> = None; // residues on the first line
        let mut ending: Option<usize> = None; // bytes in a line ending
        let mut short_seen = false;
        let mut blank_seen = false;
        pos = seq_start;
        while pos < data.len() && data[pos] != b'>' {
            let (end, next) = line_end(data, pos);
            let content_end = if end > pos && data[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            let content = &data[pos..content_end];
            pos = next;
            if content.is_empty() {
                // Blank lines after the last sequence line are harmless.
                blank_seen = true;
                continue;
            }
            if blank_seen || short_seen || memchr3(b' ', b'\t', b'\r', content).is_some() {
                regular = false;
            }
            match first {
                None => first = Some(content.len()),
                Some(width) if content.len() > width => regular = false,
                Some(width) if content.len() < width => short_seen = true,
                Some(_) => {}
            }
            if next > content_end {
                match ending {
                    None => ending = Some(next - content_end),
                    Some(bytes) if bytes != next - content_end => regular = false,
                    Some(_) => {}
                }
            }
            len += content.len();
        }

        let seq = if regular {
            let line_bases = first.unwrap_or(1);
            Seq::InPlace {
                start: seq_start,
                len,
                line_bases,
                line_bytes: line_bases + ending.unwrap_or(1),
            }
        } else {
            let residues = data[seq_start..pos]
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            copied.push(residues);
            Seq::Copied(copied.len() - 1)
        };
        records.push(Record {
            name: name_start..name_start + name_len,
            seq,
        });
    }
    Ok((records, copied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::msa::testing::TRIO;
    use std::io::Write;

    fn src(text: &str) -> Source {
        Source::from_vec(text.into(), Path::new("t.fa")).unwrap()
    }

    fn read(source: &Source, id: usize, cols: Range<usize>) -> String {
        let mut out = Vec::new();
        source.read(id, cols, &mut out);
        String::from_utf8(out).unwrap()
    }

    fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn indexes_names_and_residues() {
        let s = src(TRIO);
        assert_eq!((s.rows(), s.width(), s.min_len()), (3, 10, 10));
        assert_eq!(
            [s.name(0), s.name(1), s.name(2)],
            ["s1", "s2", "s3 third one"]
        );
        assert_eq!(read(&s, 1, 3..6), "TTA");
        assert_eq!(read(&s, 2, 0..10), "AC--TACGTA");
        assert_eq!(read(&s, 0, 8..50), "TA");
        assert!((0..3).all(|id| s.is_mapped(id)));
    }

    #[test]
    fn wrapped_records_are_read_in_place_across_line_breaks() {
        let s = src(">a\nACGT\n-ACG\nTA\n>b\nTTTT\nGGGG\nCC\n");
        assert!(s.is_mapped(0) && s.is_mapped(1));
        assert_eq!(read(&s, 0, 2..9), "GT-ACGT");
        assert_eq!(read(&s, 1, 0..10), "TTTTGGGGCC");
        assert_eq!(read(&s, 1, 4..4), "");
    }

    #[test]
    fn crlf_line_endings() {
        let s = src(&TRIO.replace('\n', "\r\n"));
        assert_eq!((s.rows(), s.width()), (3, 10));
        assert_eq!(s.name(2), "s3 third one");
        assert_eq!(read(&s, 0, 0..10), "ACGT-ACGTA");
        let w = src(">a\r\nACGT\r\n-ACG\r\nTA\r\n");
        assert_eq!(read(&w, 0, 2..9), "GT-ACGT");
        assert!(w.is_mapped(0) && s.is_mapped(2));
    }

    #[test]
    fn untidy_records_give_the_same_residues() {
        for text in [
            ">a\nACGT\n-AC\nGTA\n",
            ">a\nACGT-\n\nACGTA\n",
            ">a\nACGT- \nACGTA\t\n",
            ">a\nACGT-ACGTA",
            "\n\n>a\nACGT-ACGTA\n\n\n",
        ] {
            let s = src(text);
            assert_eq!(
                (s.rows(), s.len(0), read(&s, 0, 0..10)),
                (1, 10, "ACGT-ACGTA".to_string()),
                "{text:?}"
            );
        }
        assert!(!src(">a\nACGT\n-AC\nGTA\n").is_mapped(0));
    }

    #[test]
    fn ragged_and_empty_records() {
        let s = src(">a\nACGTACGT\n>b\n>c\nACG\n");
        assert_eq!((s.rows(), s.width(), s.min_len()), (3, 8, 0));
        assert_eq!(
            (s.len(1), read(&s, 1, 0..8), read(&s, 2, 1..8)),
            (0, String::new(), "CG".to_string())
        );
    }

    #[test]
    fn case_and_ambiguity_codes_are_kept() {
        assert_eq!(read(&src(">a\nacgtNRYn.-*X\n"), 0, 0..12), "acgtNRYn.-*X");
    }

    #[test]
    fn gzip_is_detected_by_content_including_many_members() {
        let (head, tail) = TRIO.split_at(20);
        let two_members = [gz(head.as_bytes()), gz(tail.as_bytes())].concat();
        for bytes in [gz(TRIO.as_bytes()), two_members] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("no-gz-in-the-name.fa");
            std::fs::write(&path, bytes).unwrap();
            let s = Source::load(&path).unwrap();
            assert_eq!(
                (s.rows(), read(&s, 2, 0..10)),
                (3, "AC--TACGTA".to_string())
            );
        }
    }

    #[test]
    fn plain_files_are_mapped_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.fa");
        std::fs::write(&path, TRIO).unwrap();
        let s = Source::load(&path).unwrap();
        assert_eq!(read(&s, 1, 0..10), "ACGTTACG-A");
    }

    #[test]
    fn rejects_what_is_not_fasta() {
        let message = |bytes: &[u8]| match Source::from_vec(bytes.to_vec(), Path::new("t.fa")) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("accepted {bytes:?}"),
        };
        assert_eq!(message(b""), "t.fa: no sequences found");
        assert_eq!(message(b"\n \n"), "t.fa: no sequences found");
        assert_eq!(
            message(b"\nACGT\n>a\nAC\n"),
            "t.fa: line 2 does not start with '>': not a FASTA file"
        );
        assert_eq!(
            message(b">a\nAC\n>\xff\nAC\n"),
            "t.fa: sequence 2 has a name that is not valid UTF-8"
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("e.fa"), "").unwrap();
        assert!(
            Source::load(&dir.path().join("e.fa"))
                .unwrap_err()
                .to_string()
                .ends_with("no sequences found")
        );
        assert!(matches!(Source::load(dir.path()), Err(Error::Io { .. })));
    }
}
