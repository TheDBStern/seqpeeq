//! A contig's coverage: exact, from one pass over its reads, or rough,
//! from the index alone.

use noodles::bam;
use noodles::csi::BinningIndex as _;
use noodles::csi::binning_index::index::reference_sequence::Bin;

use crate::Result;
use crate::bam::read::counted_blocks;
use crate::bam::stretch::{Union, next_start};
use crate::bam::{BamSource, Direction, Region, Stretch};
use crate::progress::Progress;

/// Counted reads between partial results handed out by the pass.
const EVERY: u64 = 65_536;

/// A contig's coverage, whole or as far as the pass has got.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub contig: usize,
    /// Mean depth over each bin; bin `i` covers `1 + i·len/n ..= (i+1)·len/n`.
    pub bins: Vec<f32>,
    /// The covered stretches up to `done_to`, inclusive.
    pub covered: Vec<(u64, u64)>,
    /// The last position whose coverage is final.
    pub done_to: u64,
}

impl Coverage {
    /// As [`next_start`], answering only where the coverage is final
    /// (`None` otherwise, when a jump has to read the BAM instead).
    pub fn next_start(&self, length: u64, from: u64, dir: Direction, want: Stretch) -> Option<u64> {
        if self.done_to >= length {
            return next_start(&self.covered, length, from, dir, want);
        }
        if dir == Direction::Back && from > self.done_to + 1 {
            return None;
        }
        next_start(&self.covered, self.done_to, from, dir, want)
    }
}

/// The first and last positions of bin `i` of `n` over a contig `len` long.
fn bin_span(i: usize, n: usize, len: u64) -> (u64, u64) {
    let (i, n) = (i as u64, n as u64);
    (1 + i * len / n, (i + 1) * len / n)
}

/// Add `count` to the bins overlapping `a..=b`, each its share by overlap
/// (`share` false adds `count` per base overlapped instead).
fn spread(sums: &mut [f64], len: u64, a: u64, b: u64, amount: f64, share: bool) {
    let n = sums.len();
    if n == 0 || len == 0 || b < a {
        return;
    }
    let width = (b - a + 1) as f64;
    let mut i = ((a.saturating_sub(1)) * n as u64 / len) as usize;
    while i < n {
        let (s, e) = bin_span(i, n, len);
        if s > b {
            break;
        }
        if e >= a && s <= e {
            let overlap = (e.min(b) - s.max(a) + 1) as f64;
            sums[i] += if share {
                amount * overlap / width
            } else {
                amount * overlap
            };
        }
        i += 1;
    }
}

impl BamSource {
    /// One pass over `contig`'s reads: mean depth in `bins` bins and the
    /// exact covered stretches. `each` sees the result so far every
    /// 65,536 counted reads, and the whole of it at the end.
    pub fn coverage(
        &self,
        contig: usize,
        bins: usize,
        progress: &Progress,
        mut each: impl FnMut(&Coverage),
    ) -> Result<Coverage> {
        let len = self.contigs[contig].length;
        let region = Region {
            contig,
            start: 1,
            end: len,
        };
        progress.set_total(len);
        let mut sums = vec![0.0f64; bins];
        let mut union = Union::default();
        let mut counted = 0;
        let mut reached = 0;
        let snapshot = |sums: &[f64], union: &Union, done_to: u64| Coverage {
            contig,
            bins: depths(sums, len),
            covered: union
                .done
                .iter()
                .filter(|&&(a, _)| a <= done_to)
                .map(|&(a, b)| (a, b.min(done_to)))
                .collect(),
            done_to,
        };
        self.scan(&region, progress, |record| {
            if let Some((start, blocks)) = counted_blocks(record)? {
                union.push(start, &blocks);
                for &(a, b) in &blocks {
                    spread(&mut sums, len, a, b, 1.0, false);
                }
                if start > reached {
                    progress.add(start - reached);
                    reached = start;
                }
                counted += 1;
                if counted % EVERY == 0 {
                    each(&snapshot(&sums, &union, start - 1));
                }
            }
            Ok(true)
        })?;
        if progress.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        let whole = Coverage {
            contig,
            bins: depths(&sums, len),
            covered: union.finish(),
            done_to: len,
        };
        each(&whole);
        Ok(whole)
    }

    /// A rough picture of where `contig`'s reads are, from the index alone:
    /// the data each index bin points at, shared out to `bins` bins by
    /// overlap and scaled so the largest is 1.0. All zeros with no reads.
    pub fn sketch(&self, contig: usize, bins: usize) -> Vec<f32> {
        let len = self.contigs[contig].length;
        let mut sums = vec![0.0f64; bins];
        let (min_shift, depth) = (self.index.min_shift(), self.index.depth());
        if let Some(index_bins) = bins_of(&self.index, contig) {
            for (&id, bin) in index_bins {
                let Some((a, b)) = bin_window(id, min_shift, depth) else {
                    continue;
                };
                let bytes: f64 = bin
                    .chunks()
                    .iter()
                    .map(|chunk| {
                        let (s, e) = (chunk.start(), chunk.end());
                        // Compressed blocks hold about three times their size.
                        let blocks = (e.compressed() - s.compressed()) as f64 * 3.0;
                        blocks + f64::from(e.uncompressed()) - f64::from(s.uncompressed())
                    })
                    .sum();
                spread(&mut sums, len, a, b.min(len), bytes.max(0.0), true);
            }
        }
        let max = sums.iter().copied().fold(0.0, f64::max);
        sums.iter()
            .map(|&s| if max > 0.0 { (s / max) as f32 } else { 0.0 })
            .collect()
    }
}

/// The 1-based stretch a binning index's bin `id` covers; `None` for the
/// pseudo-bin that holds counts.
fn bin_window(id: usize, min_shift: u8, depth: u8) -> Option<(u64, u64)> {
    let mut first = 0usize;
    for level in 0..=u32::from(depth) {
        let count = 1usize << (3 * level);
        if id < first + count {
            let size = 1u64 << (u32::from(min_shift) + 3 * (u32::from(depth) - level));
            let k = (id - first) as u64;
            return Some((k * size + 1, (k + 1) * size));
        }
        first += count;
    }
    None
}

fn depths(sums: &[f64], len: u64) -> Vec<f32> {
    let n = sums.len();
    sums.iter()
        .enumerate()
        .map(|(i, &s)| {
            let (a, b) = bin_span(i, n, len);
            if b < a {
                0.0
            } else {
                (s / (b - a + 1) as f64) as f32
            }
        })
        .collect()
}

/// The bins of `contig` in a BAI or CSI index, by id.
fn bins_of(index: &bam::Index, contig: usize) -> Option<Vec<(&usize, &Bin)>> {
    match index {
        bam::Index::Bai(index) => index
            .reference_sequences()
            .get(contig)
            .map(|r| r.bins().iter().collect()),
        bam::Index::Csi(index) => index
            .reference_sequences()
            .get(contig)
            .map(|r| r.bins().iter().collect()),
    }
}
