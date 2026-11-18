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
                let mut indexer = fasta::io::Indexer::new(BufReader::new(file));
                let mut records = Vec::new();
                while let Some(record) = indexer.index_record().map_err(|e| Error::Format {
                    path: path.to_path_buf(),
                    message: format!("not a readable FASTA: {e}"),
                })? {
                    if progress.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    records.push(record);
                }
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
                if b != b'\n' && b != b'\r' {
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
