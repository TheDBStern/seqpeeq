//! A reference FASTA attached to a BAM, read a stretch at a time.

use std::fs::File;
use std::io::{self, BufReader, Read as _};
use std::path::{Path, PathBuf};

use noodles::core::{Position, Region as CoreRegion};
use noodles::fasta;

use crate::bam::index::file_name;
use crate::progress::Progress;
use crate::{Error, Result};

/// A reference FASTA and its index: plain (its `.fai` read, or built in
/// memory) or bgzipped (with its `.fai` and `.gzi`).
#[derive(Debug, Clone)]
pub struct Reference {
    path: PathBuf,
    index: fasta::fai::Index,
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
        let index = match packing(path).map_err(failed)? {
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
        Ok(Reference {
            path: path.to_path_buf(),
            index,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the FASTA has a sequence called `name`.
    pub fn has(&self, name: &str) -> bool {
        self.index.as_ref().iter().any(|r| r.name() == name)
    }

    /// The bases of `name` from `start` to `end`, 1-based and inclusive.
    pub fn fetch(&self, name: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        let failed = |cause: io::Error| Error::Io {
            path: self.path.clone(),
            cause,
        };
        let mut reader = fasta::io::indexed_reader::Builder::default()
            .set_index(self.index.clone())
            .build_from_path(&self.path)
            .map_err(failed)?;
        let position = |p: u64| {
            Position::try_from(p as usize)
                .map_err(|_| Error::Invalid("positions count from 1".to_string()))
        };
        let region = CoreRegion::new(name, position(start)?..=position(end)?);
        let record = reader.query(&region).map_err(failed)?;
        Ok(record.sequence().as_ref().to_vec())
    }
}
