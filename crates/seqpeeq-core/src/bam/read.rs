//! The alignments of a region, as the view and the coverage need them.

use std::fs::File;
use std::io;

use noodles::bam;
use noodles::core::{Position, Region as CoreRegion};
use noodles::sam::alignment::Record as _;
use noodles::sam::alignment::record::cigar::op::Kind as OpKind;
use noodles::sam::alignment::record::data::field::{Tag, Value};

use crate::bam::{BamSource, Region};
use crate::progress::Progress;
use crate::{Error, Result};

/// Records read between checks for cancelling.
const BATCH: usize = 4096;

/// Where an alignment starts: a contig (by its place in the header), a
/// 1-based position, and its strand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub contig: usize,
    pub pos: u64,
    pub reverse: bool,
}

/// Another part of a split read, from the SA tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub place: Place,
    pub cigar: String,
    pub mapq: u8,
}

/// One mapped alignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub name: String,
    pub flags: u16,
    pub contig: usize,
    /// The first and last reference positions it covers, 1-based.
    pub start: u64,
    pub end: u64,
    pub mapq: u8,
    pub cigar: Vec<(char, u32)>,
    /// The bases as stored (on the reference's strand); empty for SEQ `*`.
    pub seq: Vec<u8>,
    pub md: Option<String>,
    /// Where the mate is, when it is mapped.
    pub mate: Option<Place>,
    pub parts: Vec<Part>,
}

const UNMAPPED: u16 = 0x4;
const REVERSE: u16 = 0x10;
const SECONDARY: u16 = 0x100;
const QC_FAIL: u16 = 0x200;
const DUPLICATE: u16 = 0x400;

impl Read {
    /// Whether it counts towards coverage: mapped, and not secondary,
    /// QC-failed or a duplicate (as samtools depth counts).
    pub fn counts(&self) -> bool {
        self.flags & (UNMAPPED | SECONDARY | QC_FAIL | DUPLICATE) == 0
    }

    pub fn reverse(&self) -> bool {
        self.flags & REVERSE != 0
    }

    /// The reference stretches its M, = and X operations cover, inclusive,
    /// with touching stretches joined.
    pub fn blocks(&self) -> Vec<(u64, u64)> {
        blocks(self.start, &self.cigar)
    }
}

/// The inclusive reference stretches that `cigar`'s M, = and X operations
/// cover, from `start`.
pub(crate) fn blocks(start: u64, cigar: &[(char, u32)]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut pos = start;
    for &(op, len) in cigar {
        let len = u64::from(len);
        match op {
            'M' | '=' | 'X' if len > 0 => {
                match out.last_mut() {
                    Some(last) if last.1 + 1 == pos => last.1 = pos + len - 1,
                    _ => out.push((pos, pos + len - 1)),
                }
                pos += len;
            }
            'D' | 'N' => pos += len,
            _ => {}
        }
    }
    out
}

fn op_char(kind: OpKind) -> char {
    match kind {
        OpKind::Match => 'M',
        OpKind::Insertion => 'I',
        OpKind::Deletion => 'D',
        OpKind::Skip => 'N',
        OpKind::SoftClip => 'S',
        OpKind::HardClip => 'H',
        OpKind::Pad => 'P',
        OpKind::SequenceMatch => '=',
        OpKind::SequenceMismatch => 'X',
    }
}

impl BamSource {
    /// Every record noodles' query returns for `region`, mapped or not, in
    /// file order.
    pub(crate) fn records(&self, region: &Region, progress: &Progress) -> Result<Vec<bam::Record>> {
        let failed = |cause: io::Error| Error::Io {
            path: self.path.clone(),
            cause,
        };
        let mut reader = File::open(&self.path)
            .map(bam::io::Reader::new)
            .map_err(failed)?;
        reader.read_header().map_err(failed)?;
        let name = self.contigs[region.contig].name.as_bytes();
        let position = |p: u64| Position::try_from(p as usize).expect("positions count from 1");
        let core = CoreRegion::new(name, position(region.start)..=position(region.end));
        let query = reader
            .query(&self.header, &*self.index, &core)
            .map_err(failed)?;
        let mut records = Vec::new();
        for (i, record) in query.records().enumerate() {
            if i % BATCH == BATCH - 1 && progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            records.push(record.map_err(failed)?);
        }
        Ok(records)
    }

    /// The mapped alignments overlapping `region`, in file order.
    pub fn reads(&self, region: &Region, progress: &Progress) -> Result<Vec<Read>> {
        let records = self.records(region, progress)?;
        let mut reads = Vec::with_capacity(records.len());
        for record in &records {
            if record.flags().is_unmapped() {
                continue;
            }
            reads.push(self.read(record).map_err(|cause| Error::Io {
                path: self.path.clone(),
                cause,
            })?);
        }
        Ok(reads)
    }

    /// One mapped record as a [`Read`].
    pub(crate) fn read(&self, record: &bam::Record) -> io::Result<Read> {
        let flags = record.flags();
        let contig = record.reference_sequence_id().transpose()?.unwrap_or(0);
        let start = record
            .alignment_start()
            .transpose()?
            .map_or(0, |p| p.get() as u64);
        let end = record
            .alignment_end()
            .transpose()?
            .map_or(start, |p| p.get() as u64);
        let cigar = record
            .cigar()
            .iter()
            .map(|op| op.map(|op| (op_char(op.kind()), op.len() as u32)))
            .collect::<io::Result<Vec<_>>>()?;
        let data = record.data();
        let text = |tag: Tag| -> io::Result<Option<String>> {
            match data.get(&tag).transpose()? {
                Some(Value::String(s)) => Ok(Some(s.to_string())),
                _ => Ok(None),
            }
        };
        let mate = match (
            flags.is_segmented() && !flags.is_mate_unmapped(),
            record.mate_reference_sequence_id().transpose()?,
            record.mate_alignment_start().transpose()?,
        ) {
            (true, Some(contig), Some(pos)) => Some(Place {
                contig,
                pos: pos.get() as u64,
                reverse: flags.is_mate_reverse_complemented(),
            }),
            _ => None,
        };
        let parts = text(Tag::OTHER_ALIGNMENTS)?
            .map(|sa| self.parse_sa(&sa))
            .unwrap_or_default();
        Ok(Read {
            name: record.name().map(|n| n.to_string()).unwrap_or_default(),
            flags: flags.bits(),
            contig,
            start,
            end,
            mapq: record.mapping_quality().map_or(255, |q| q.get()),
            cigar,
            seq: record.sequence().iter().collect(),
            md: text(Tag::MISMATCHED_POSITIONS)?,
            mate,
            parts,
        })
    }

    /// The parts an SA tag lists, leaving out any on a contig this BAM's
    /// header does not name.
    fn parse_sa(&self, sa: &str) -> Vec<Part> {
        sa.split(';')
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| {
                let fields: Vec<&str> = entry.split(',').collect();
                let [name, pos, strand, cigar, mapq, ..] = fields.as_slice() else {
                    return None;
                };
                let contig = self.contigs.iter().position(|c| c.name == *name)?;
                Some(Part {
                    place: Place {
                        contig,
                        pos: pos.parse().ok()?,
                        reverse: *strand == "-",
                    },
                    cigar: (*cigar).to_string(),
                    mapq: mapq.parse().ok()?,
                })
            })
            .collect()
    }
}
