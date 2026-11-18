//! The depth and reads around what the view shows.

use crate::Result;
use crate::bam::read::{blocks, counted};
use crate::bam::{BamSource, Read, Region};
use crate::progress::Progress;

/// What was read of one stretch of a contig.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub region: Region,
    /// Bases per bin: bin `i` covers `region.start + i·bin ..=
    /// region.start + (i+1)·bin - 1`, cut at `region.end`.
    pub bin: u64,
    /// Mean depth of counted reads' aligned bases in each bin.
    pub depth: Vec<f32>,
    /// Every mapped read overlapping, in file order; `None` when not asked
    /// for or when there were more than the limit.
    pub reads: Option<Vec<Read>>,
    /// The mapped reads overlapping the region.
    pub count: usize,
}

impl BamSource {
    /// Read `region` once: depth in bins of `bin` bases, and its reads
    /// when `limit` is given and there are no more than that.
    pub fn window(
        &self,
        region: &Region,
        bin: u64,
        limit: Option<usize>,
        progress: &Progress,
    ) -> Result<Window> {
        let bin = bin.max(1);
        let width = region.end.saturating_sub(region.start) + 1;
        let bins = width.div_ceil(bin) as usize;
        let mut sums = vec![0u64; bins];
        let mut reads = limit.map(|_| Vec::new());
        let mut count = 0;
        let mut reached = region.start;
        progress.set_total(width);
        self.scan(region, progress, |record| {
            if record.flags().is_unmapped() {
                return Ok(true);
            }
            count += 1;
            if let Some((start, cigar)) = counted(record)? {
                for (a, b) in blocks(start, &cigar) {
                    let (a, b) = (a.max(region.start), b.min(region.end));
                    let mut at = a;
                    while at <= b {
                        let i = ((at - region.start) / bin) as usize;
                        let bin_end = region.start + (i as u64 + 1) * bin - 1;
                        let to = bin_end.min(b);
                        sums[i] += to - at + 1;
                        at = to + 1;
                    }
                }
                if start > reached {
                    progress.add(start.min(region.end) - reached);
                    reached = start.min(region.end);
                }
            }
            if let (Some(kept), Some(limit)) = (&mut reads, limit) {
                if count > limit {
                    reads = None;
                } else {
                    kept.push(self.read(record)?);
                }
            }
            Ok(true)
        })?;
        if progress.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        progress.add(region.end + 1 - reached);
        let depth = sums
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let a = region.start + i as u64 * bin;
                let b = (a + bin - 1).min(region.end);
                s as f32 / (b - a + 1) as f32
            })
            .collect();
        Ok(Window {
            region: *region,
            bin,
            depth,
            reads,
            count,
        })
    }
}
