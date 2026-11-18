//! A read's other placements: its mate and split parts through the index,
//! and every record of some names through the whole file.

use std::collections::HashSet;
use std::fs::File;
use std::io;

use noodles::bam;

use super::export_reads::{name_of, start_of};
use super::read::BATCH;
use super::{BamSource, Read, Region, segment};
use crate::progress::Progress;
use crate::{Error, Result};

const SECONDARY: u16 = 0x100;
const SUPPLEMENTARY: u16 = 0x800;

impl BamSource {
    /// The first record placed at `contig:pos` that `want` accepts.
    pub(crate) fn record_at(
        &self,
        contig: usize,
        pos: u64,
        progress: &Progress,
        want: impl Fn(&bam::Record) -> bool,
    ) -> Result<Option<bam::Record>> {
        let Some(length) = self.contigs.get(contig).map(|c| c.length) else {
            return Ok(None);
        };
        if pos == 0 || pos > length {
            return Ok(None);
        }
        let region = Region {
            contig,
            start: pos,
            end: pos,
        };
        let mut found = None;
        self.scan(&region, progress, |record| {
            if start_of(record)? == Some(pos) && want(record) {
                found = Some(record.clone());
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(found)
    }

    /// The records of `read`'s other placements that something points to:
    /// its mate (by the mate fields), then the split parts of the read and
    /// of the mate (by their SA tags), each fetched through the index. Not
    /// `read` itself, and each record once.
    pub fn gather(&self, read: &Read, progress: &Progress) -> Result<Vec<Read>> {
        let name = read.name.as_bytes();
        let mut seen = HashSet::from([(read.flags, read.contig, read.start)]);
        let mut out: Vec<Read> = Vec::new();
        let mut take = |record: bam::Record, out: &mut Vec<Read>| -> Result<()> {
            let found = self.read(&record).map_err(|cause| self.io_error(cause))?;
            if seen.insert((found.flags, found.contig, found.start)) {
                out.push(found);
            }
            Ok(())
        };
        let own = segment(read.flags);
        if let (Some(mate), 1 | 2) = (read.mate, own) {
            let other = 3 - own;
            let want = |r: &bam::Record| {
                let f = r.flags().bits();
                name_of(r) == name && segment(f) == other && f & (SECONDARY | SUPPLEMENTARY) == 0
            };
            if let Some(record) = self.record_at(mate.contig, mate.pos, progress, want)? {
                take(record, &mut out)?;
            }
        }
        let carriers: Vec<Read> = std::iter::once(read.clone()).chain(out.clone()).collect();
        for carrier in &carriers {
            let seg = segment(carrier.flags);
            for part in &carrier.parts {
                let want = |r: &bam::Record| {
                    let f = r.flags().bits();
                    name_of(r) == name
                        && segment(f) == seg
                        && f & SECONDARY == 0
                        && r.flags().is_reverse_complemented() == part.place.reverse
                };
                if let Some(record) =
                    self.record_at(part.place.contig, part.place.pos, progress, want)?
                {
                    take(record, &mut out)?;
                }
            }
        }
        Ok(out)
    }

    /// Every record in the file whose name is one of `names`, mapped or
    /// not, in file order. Reads the whole BAM.
    pub fn search_names(&self, names: &HashSet<String>, progress: &Progress) -> Result<Vec<Read>> {
        let names: HashSet<&[u8]> = names.iter().map(|n| n.as_bytes()).collect();
        let mut out = Vec::new();
        self.each_record(progress, |record| {
            if names.contains(name_of(record).as_slice()) {
                out.push(self.read(record)?);
            }
            Ok(true)
        })?;
        Ok(out)
    }

    /// Hand every record in the file to `each`, in file order, until it
    /// says to stop. Progress counts records against the index's counts.
    pub(crate) fn each_record(
        &self,
        progress: &Progress,
        mut each: impl FnMut(&bam::Record) -> io::Result<bool>,
    ) -> Result<()> {
        if progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let total: Option<u64> = self
            .contigs
            .iter()
            .map(|c| Some(c.mapped? + c.unmapped?))
            .sum::<Option<u64>>()
            .map(|placed| placed + self.unplaced.unwrap_or(0));
        if let Some(total) = total {
            progress.set_total(total);
        }
        let mut reader = File::open(&self.path)
            .map(bam::io::Reader::new)
            .map_err(|cause| self.io_error(cause))?;
        reader.read_header().map_err(|cause| self.io_error(cause))?;
        let mut since = 0u64;
        for record in reader.records() {
            let record = record.map_err(|cause| self.io_error(cause))?;
            since += 1;
            if since == BATCH as u64 {
                progress.add(since);
                since = 0;
                if progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
            }
            if !each(&record).map_err(|cause| self.io_error(cause))? {
                break;
            }
        }
        progress.add(since);
        Ok(())
    }

    fn io_error(&self, cause: io::Error) -> Error {
        Error::Io {
            path: self.path.clone(),
            cause,
        }
    }
}
