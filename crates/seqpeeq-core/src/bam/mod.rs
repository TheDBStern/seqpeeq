//! Mapped reads: sorted BAM files, read by region through their index.

mod axis;
mod coverage;
mod export;
mod export_reads;
mod gather;
mod index;
mod pieces;
mod read;
mod reference;
mod region;
mod stretch;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;
#[cfg(test)]
mod tests;
mod window;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use noodles::bam;
use noodles::csi::BinningIndex as _;
use noodles::sam;

use crate::progress::Progress;
use crate::{Error, Result};

pub use axis::{parse_cigar, read_span, segment};
pub use coverage::Coverage;
pub(crate) use export::BamExport;
pub use pieces::Piece;
pub use read::{Alt, Part, Place, Read};
pub use reference::Reference;
pub use region::{Region, parse_region};
pub use stretch::{Direction, Stretch, next_start};
pub use window::Window;

/// A reference sequence named in a BAM's header, with the read counts its
/// index holds (`None` when the index keeps no counts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contig {
    pub name: String,
    pub length: u64,
    pub mapped: Option<u64>,
    pub unmapped: Option<u64>,
}

/// `n` with commas between groups of three digits: `1,204,551`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

/// An open BAM: what reading it needs, and what the open said about it.
#[derive(Debug)]
pub struct BamDoc {
    source: BamSource,
    notes: Vec<String>,
}

/// What reading a BAM needs, cheap to clone and safe to share with other
/// threads, so a view can read regions while the session goes on.
#[derive(Debug, Clone)]
pub struct BamSource {
    path: PathBuf,
    header: Arc<sam::Header>,
    index: Arc<bam::Index>,
    contigs: Arc<[Contig]>,
    /// Reads placed nowhere, from the index; `None` when it keeps no count.
    unplaced: Option<u64>,
    reference: Option<Arc<Reference>>,
}

impl BamDoc {
    /// Read the header and the index, building the index if there is none
    /// (or only one older than the BAM). `cache` is where an index goes
    /// when the BAM's folder cannot take it.
    pub(crate) fn open(path: &Path, cache: &Path, progress: &Progress) -> Result<BamDoc> {
        let header = bam::io::reader::Builder
            .build_from_path(path)
            .and_then(|mut reader| reader.read_header())
            .map_err(|e| Error::Format {
                path: path.to_path_buf(),
                message: format!("not a readable BAM: {e}"),
            })?;
        if index::says_unsorted(&header) {
            return Err(index::unsorted(path));
        }
        let (index, note) = index::load_or_build(path, cache, progress)?;
        let counts: Vec<Option<(u64, u64)>> = index
            .reference_sequences()
            .map(|r| {
                r.metadata()
                    .map(|m| (m.mapped_record_count(), m.unmapped_record_count()))
            })
            .collect();
        // A contig with no reads has no counts either; only an index with
        // no counts anywhere keeps none.
        let known = counts.iter().any(Option::is_some);
        let contigs: Arc<[Contig]> = header
            .reference_sequences()
            .iter()
            .enumerate()
            .map(|(i, (name, map))| {
                let (mapped, unmapped) = match counts.get(i).copied().flatten() {
                    Some((m, u)) => (Some(m), Some(u)),
                    None if known => (Some(0), Some(0)),
                    None => (None, None),
                };
                Contig {
                    name: name.to_string(),
                    length: map.length().get() as u64,
                    mapped,
                    unmapped,
                }
            })
            .collect();
        let unplaced = known.then(|| index.unplaced_unmapped_record_count().unwrap_or(0));
        Ok(BamDoc {
            source: BamSource {
                path: path.to_path_buf(),
                header: Arc::new(header),
                index: Arc::new(index),
                contigs,
                unplaced,
                reference: None,
            },
            notes: note.into_iter().collect(),
        })
    }

    pub fn source(&self) -> &BamSource {
        &self.source
    }

    /// What the open message says it holds:
    /// `3 contigs, 10 mapped reads, 2 unmapped`.
    pub fn shape(&self) -> String {
        let contigs = self.source.contigs();
        let n = format!(
            "{} contig{}",
            grouped(contigs.len() as u64),
            if contigs.len() == 1 { "" } else { "s" }
        );
        let mapped: Option<u64> = contigs.iter().map(|c| c.mapped).sum();
        let unmapped: Option<u64> = contigs.iter().map(|c| c.unmapped).sum();
        match (mapped, unmapped, self.source.unplaced) {
            (Some(m), Some(u), Some(p)) => format!(
                "{n}, {} mapped read{}, {} unmapped",
                grouped(m),
                if m == 1 { "" } else { "s" },
                grouped(u + p)
            ),
            _ => format!("{n}; the index has no read counts"),
        }
    }

    /// Notes from the open, such as where a built index went.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Use `reference` for reference bases from now on. Returns what the
    /// message says: `attached ref.fa to reads.bam (3 of 3 contigs)`.
    pub(crate) fn set_reference(&mut self, reference: Reference) -> String {
        let contigs = self.source.contigs();
        let found = contigs.iter().filter(|c| reference.has(&c.name)).count();
        let missing = contigs.len() - found;
        let ref_name = index::file_name(reference.path());
        let mut message = format!(
            "attached {ref_name} to {} ({} of {} contigs",
            self.source.file_name(),
            grouped(found as u64),
            grouped(contigs.len() as u64)
        );
        if missing > 0 {
            message.push_str(&format!(
                "; {} {} not in {ref_name}",
                grouped(missing as u64),
                if missing == 1 { "is" } else { "are" }
            ));
        }
        message.push(')');
        self.source.reference = Some(Arc::new(reference));
        message
    }
}

impl BamSource {
    pub fn contigs(&self) -> &[Contig] {
        &self.contigs
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The reference attached with `bam ref`, if any.
    pub fn reference(&self) -> Option<&Reference> {
        self.reference.as_deref()
    }

    /// The BAM's name without its folder, for messages.
    pub fn file_name(&self) -> String {
        index::file_name(&self.path)
    }
}
