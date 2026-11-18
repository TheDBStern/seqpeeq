//! Covered and empty stretches of a contig, and the jumps between them.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::Result;
use crate::bam::read::{counted, spans};
use crate::bam::{BamSource, Region};
use crate::progress::Progress;

/// The kind of stretch a jump looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stretch {
    /// Positions where at least one counted read has an aligned base.
    Covered,
    /// Positions where none has.
    Empty,
}

/// Which way a jump goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Back,
}

/// The covered intervals of reads fed in order of their start, joined
/// where they touch or overlap. An interval is settled (in `done`) once a
/// read starting past it has been fed, because no later read can reach
/// back before its own start.
#[derive(Debug, Default)]
pub(crate) struct Union {
    pending: BinaryHeap<Reverse<(u64, u64)>>,
    pub(crate) done: Vec<(u64, u64)>,
}

impl Union {
    /// Feed the blocks of a read starting at `start`; reads come in order
    /// of their start.
    pub(crate) fn push(&mut self, start: u64, blocks: &[(u64, u64)]) {
        self.settle_before(start);
        self.pending.extend(blocks.iter().map(|&b| Reverse(b)));
    }

    /// Move every pending block starting before `pos` into `done`.
    fn settle_before(&mut self, pos: u64) {
        while let Some(&Reverse((a, b))) = self.pending.peek() {
            if a >= pos {
                break;
            }
            self.pending.pop();
            match self.done.last_mut() {
                Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
                _ => self.done.push((a, b)),
            }
        }
    }

    /// All the intervals, once every read has been fed.
    pub(crate) fn finish(mut self) -> Vec<(u64, u64)> {
        self.settle_before(u64::MAX);
        self.done
    }
}

/// The starts of a contig's stretches, in order, each with its kind:
/// covered and empty stretches alternate and together cover `1..=length`.
fn starts(covered: &[(u64, u64)], length: u64) -> impl Iterator<Item = (u64, Stretch)> + '_ {
    let mut pos = 1;
    let mut out = Vec::with_capacity(covered.len() * 2 + 1);
    for &(a, b) in covered {
        if a > pos {
            out.push((pos, Stretch::Empty));
        }
        out.push((a, Stretch::Covered));
        pos = b + 1;
    }
    if pos <= length {
        out.push((pos, Stretch::Empty));
    }
    out.into_iter()
}

/// The start of the nearest stretch of kind `want` beginning strictly
/// after `from` (Forward) or strictly before it (Back), given the covered
/// intervals of a contig `length` long.
pub fn next_start(
    covered: &[(u64, u64)],
    length: u64,
    from: u64,
    dir: Direction,
    want: Stretch,
) -> Option<u64> {
    let mut wanted = starts(covered, length).filter(|&(_, kind)| kind == want);
    match dir {
        Direction::Forward => wanted.find(|&(p, _)| p > from).map(|(p, _)| p),
        Direction::Back => wanted.take_while(|&(p, _)| p < from).last().map(|(p, _)| p),
    }
}

/// The first window a backward jump reads, in bases; it doubles until the
/// answer is found or the window reaches the contig's start.
const BACK_WINDOW: u64 = 65_536;

impl BamSource {
    /// Where the nearest stretch of kind `want` starts after (or before)
    /// `from` on `contig`, reading only as much of the BAM as that needs.
    pub fn jump(
        &self,
        contig: usize,
        from: u64,
        dir: Direction,
        want: Stretch,
        progress: &Progress,
    ) -> Result<Option<u64>> {
        let length = self.contigs[contig].length;
        // Positions count from 1; "after 0" is "from the start".
        match dir {
            Direction::Forward => self.jump_forward(contig, length, from, want, progress),
            Direction::Back => self.jump_back(contig, length, from, want, progress),
        }
    }

    fn jump_forward(
        &self,
        contig: usize,
        length: u64,
        from: u64,
        want: Stretch,
        progress: &Progress,
    ) -> Result<Option<u64>> {
        if progress.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        if from >= length {
            return Ok(None);
        }
        let region = Region {
            contig,
            start: from.max(1),
            end: length,
        };
        let mut union = Union::default();
        let mut found = None;
        // Coverage at positions from `from` on is exact, so a stretch
        // starting after `from` is known once the intervals around it
        // are settled.
        self.scan(&region, progress, |record| {
            if let Some((start, cigar)) = counted(record)? {
                union.push(start, &spans(start, &cigar));
                found = settled_answer(&union.done, from, want);
            }
            Ok(found.is_none())
        })?;
        if found.is_some() {
            return Ok(found);
        }
        Ok(next_start(
            &union.finish(),
            length,
            from,
            Direction::Forward,
            want,
        ))
    }

    fn jump_back(
        &self,
        contig: usize,
        length: u64,
        from: u64,
        want: Stretch,
        progress: &Progress,
    ) -> Result<Option<u64>> {
        if progress.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        let from = from.min(length);
        if from <= 1 {
            return Ok(None);
        }
        let mut width = BACK_WINDOW;
        loop {
            let start = from.saturating_sub(width).max(1);
            let region = Region {
                contig,
                start,
                end: from,
            };
            let mut union = Union::default();
            self.scan(&region, progress, |record| {
                if let Some((read_start, cigar)) = counted(record)? {
                    let clipped: Vec<(u64, u64)> = spans(read_start, &cigar)
                        .iter()
                        .filter(|&&(a, b)| b >= start && a <= from)
                        .map(|&(a, b)| (a.max(start), b.min(from)))
                        .collect();
                    union.push(read_start.max(start), &clipped);
                }
                Ok(true)
            })?;
            let covered = union.finish();
            // Within the window, a stretch start is known only past its
            // first base, unless the window begins the contig.
            let found = starts(&covered, from)
                .filter(|&(p, kind)| kind == want && p < from && (p > start || start == 1))
                .last()
                .map(|(p, _)| p);
            if found.is_some() || start == 1 {
                return Ok(found);
            }
            width = width.saturating_mul(2);
        }
    }
}

/// The answer a forward jump can give from the settled intervals alone:
/// a covered stretch is settled once its interval is, and an empty one once
/// the interval after it is.
fn settled_answer(done: &[(u64, u64)], from: u64, want: Stretch) -> Option<u64> {
    match want {
        Stretch::Covered => done.iter().map(|&(a, _)| a).find(|&a| a > from),
        Stretch::Empty => done
            .windows(2)
            .map(|pair| pair[0].1 + 1)
            .find(|&p| p > from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_start_walks_stretches_both_ways() {
        use Direction::{Back, Forward};
        use Stretch::{Covered, Empty};
        let c1 = [(101, 150), (156, 165), (301, 350), (701, 740), (901, 950)];
        let n = |from, dir, want| next_start(&c1, 1000, from, dir, want);
        assert_eq!(n(1, Forward, Covered), Some(101));
        assert_eq!(n(101, Forward, Covered), Some(156));
        assert_eq!(n(160, Forward, Covered), Some(301));
        assert_eq!(n(950, Forward, Covered), None);
        assert_eq!(n(400, Back, Covered), Some(301));
        assert_eq!(n(301, Back, Covered), Some(156));
        assert_eq!(n(101, Back, Covered), None);
        assert_eq!(n(1, Forward, Empty), Some(151));
        assert_eq!(n(120, Forward, Empty), Some(151));
        assert_eq!(n(960, Forward, Empty), None);
        assert_eq!(n(160, Back, Empty), Some(151));
        assert_eq!(n(151, Back, Empty), Some(1));
        assert_eq!(n(1, Back, Empty), None);
        // A contig with no reads is one empty stretch.
        assert_eq!(next_start(&[], 300, 1, Forward, Covered), None);
        assert_eq!(next_start(&[], 300, 50, Back, Empty), Some(1));
        // A contig covered to its end has no empty stretch after the last read.
        assert_eq!(next_start(&[(1, 300)], 300, 5, Forward, Empty), None);
        assert_eq!(next_start(&[(1, 300)], 300, 5, Back, Covered), Some(1));
    }

    #[test]
    fn a_union_joins_touching_and_overlapping_blocks_in_order() {
        let mut union = Union::default();
        union.push(101, &[(101, 150)]);
        union.push(121, &[(121, 150), (156, 165)]);
        union.push(151, &[(151, 155)]);
        union.push(301, &[(301, 350)]);
        assert_eq!(union.finish(), [(101, 165), (301, 350)]);
    }
}
