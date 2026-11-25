//! Where a window's reads differ from the reference: each read's edits,
//! and per-position counts of alleles, deletions and insertions.

use std::collections::HashMap;

use crate::bam::{Piece, Read};

/// The fewest reads a position's strongest signal needs to count.
pub const MIN_READS: u32 = 3;
/// The smallest share of a position's reads its strongest signal needs.
pub const MIN_FRACTION: f32 = 0.15;

/// One way a read differs from the reference at a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// An aligned base that is not the reference's: the read's base.
    Mismatch(u8),
    /// A deleted reference base.
    Deleted,
    /// Bases inserted just before this position.
    Inserted,
}

/// The reference's bases from `start`.
#[derive(Debug, Clone, Copy)]
pub struct RefSlice<'a> {
    pub start: u64,
    pub bases: &'a [u8],
}

impl RefSlice<'_> {
    fn at(&self, pos: u64) -> Option<u8> {
        let i = usize::try_from(pos.checked_sub(self.start)?).ok()?;
        self.bases.get(i).map(u8::to_ascii_uppercase)
    }
}

/// Walk `read`'s aligned bases, deletions and insertions, telling `each`
/// the position, the reference base there when known, and the edit (`None`
/// for a base that matches). False, having told `each` nothing, when
/// neither `reference` nor the read's MD tag says what the reference is.
fn walk(
    read: &Read,
    reference: Option<RefSlice<'_>>,
    mut each: impl FnMut(u64, Option<u8>, Option<Edit>),
) -> bool {
    if read.seq.is_empty() {
        return false;
    }
    let md: Option<HashMap<u64, u8>> = match reference {
        Some(_) => None,
        None => match read.md_reference() {
            Some(bases) => Some(bases.into_iter().collect()),
            None => return false,
        },
    };
    let ref_at = |pos: u64| match (&reference, &md) {
        (Some(r), _) => r.at(pos),
        (None, Some(m)) => m.get(&pos).copied(),
        (None, None) => None,
    };
    for piece in read.pieces() {
        match piece {
            Piece::Base { pos, base } => {
                let r = ref_at(pos);
                let edit = match (base, r) {
                    // `=` is SEQ's way of saying "as the reference".
                    (Some(b), Some(r))
                        if !b.eq_ignore_ascii_case(&r)
                            && !b.eq_ignore_ascii_case(&b'N')
                            && b != b'=' =>
                    {
                        Some(Edit::Mismatch(b.to_ascii_uppercase()))
                    }
                    _ => None,
                };
                each(pos, r, edit);
            }
            Piece::Deleted { pos } => each(pos, ref_at(pos), Some(Edit::Deleted)),
            Piece::Inserted { before, .. } => each(before, None, Some(Edit::Inserted)),
            Piece::Skipped { .. } | Piece::Clipped { .. } => {}
        }
    }
    true
}

/// Where `read` differs from the reference, in the order its CIGAR gives:
/// one entry per deleted base, one per insertion. `None` when neither
/// `reference` nor its MD tag says what the reference is.
pub fn edits(read: &Read, reference: Option<RefSlice<'_>>) -> Option<Vec<(u64, Edit)>> {
    let mut out = Vec::new();
    walk(read, reference, |pos, _, edit| {
        if let Some(edit) = edit {
            out.push((pos, edit));
        }
    })
    .then_some(out)
}

/// What the reads say at one position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// The reference base, upper case, when known.
    pub reference: Option<u8>,
    /// Reads with an aligned base here.
    pub aligned: u32,
    /// Mismatching bases by the read's base: A, C, G, T.
    pub alt: [u32; 4],
    /// Reads with this base deleted.
    pub deleted: u32,
    /// Reads with bases inserted just before this position.
    pub inserted: u32,
}

impl Counts {
    /// Reads with a base or a deletion here: what fractions are of.
    pub fn depth(&self) -> u32 {
        self.aligned + self.deleted
    }

    /// The strongest departure from the reference: its read count and
    /// whether it is an indel (which wins a tie).
    fn top(&self) -> (u32, bool) {
        let snp = self.alt.iter().copied().max().unwrap_or(0);
        let indel = self.deleted.max(self.inserted);
        if indel >= snp {
            (indel, true)
        } else {
            (snp, false)
        }
    }
}

/// A position's strongest departure from the reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Signal {
    pub pos: u64,
    /// Its share of the reads with a base or a deletion there.
    pub fraction: f32,
    pub indel: bool,
}

/// How far the reads differ from the reference over a stretch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// Mismatches per aligned base.
    pub mismatch_rate: f64,
    /// Deletions and insertions per 1,000 aligned bases.
    pub indels_per_kb: f64,
}

/// Per-position counts over `start..=end` of a window's counted reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Tally {
    start: u64,
    /// The reference base, upper case, or 0 when unknown.
    reference: Vec<u8>,
    aligned: Vec<u32>,
    alt: Vec<[u32; 4]>,
    deleted: Vec<u32>,
    deletion_starts: Vec<u32>,
    inserted: Vec<u32>,
}

impl Tally {
    /// Count `reads` (those that count towards coverage) over `start..=end`.
    /// `None` without `reference` when no counted read's MD tag said what
    /// the reference is.
    pub fn of(
        reads: &[Read],
        reference: Option<RefSlice<'_>>,
        start: u64,
        end: u64,
    ) -> Option<Tally> {
        let n = usize::try_from((end + 1).saturating_sub(start)).ok()?;
        let mut t = Tally {
            start,
            reference: vec![0; n],
            aligned: vec![0; n],
            alt: vec![[0; 4]; n],
            deleted: vec![0; n],
            deletion_starts: vec![0; n],
            inserted: vec![0; n],
        };
        if let Some(r) = reference {
            for (i, b) in t.reference.iter_mut().enumerate() {
                *b = r.at(start + i as u64).unwrap_or(0);
            }
        }
        let mut any = false;
        for read in reads.iter().filter(|r| r.counts()) {
            let mut last_deleted = None;
            any |= walk(read, reference, |pos, base, edit| {
                let Some(i) = t.index(pos) else {
                    return;
                };
                if let Some(b) = base
                    && t.reference[i] == 0
                {
                    t.reference[i] = b;
                }
                match edit {
                    None => t.aligned[i] += 1,
                    Some(Edit::Mismatch(b)) => {
                        t.aligned[i] += 1;
                        if let Some(k) = b"ACGT".iter().position(|&x| x == b) {
                            t.alt[i][k] += 1;
                        }
                    }
                    Some(Edit::Deleted) => {
                        t.deleted[i] += 1;
                        if last_deleted != Some(pos.wrapping_sub(1)) {
                            t.deletion_starts[i] += 1;
                        }
                        last_deleted = Some(pos);
                    }
                    Some(Edit::Inserted) => t.inserted[i] += 1,
                }
            });
        }
        (any || reference.is_some()).then_some(t)
    }

    fn index(&self, pos: u64) -> Option<usize> {
        let i = usize::try_from(pos.checked_sub(self.start)?).ok()?;
        (i < self.aligned.len()).then_some(i)
    }

    /// The counts at `pos`; `None` outside the tally's stretch.
    pub fn counts(&self, pos: u64) -> Option<Counts> {
        let i = self.index(pos)?;
        Some(Counts {
            reference: (self.reference[i] != 0).then_some(self.reference[i]),
            aligned: self.aligned[i],
            alt: self.alt[i],
            deleted: self.deleted[i],
            inserted: self.inserted[i],
        })
    }

    /// The signal at `pos`, when it clears [`MIN_READS`] and
    /// [`MIN_FRACTION`].
    fn signal(&self, pos: u64) -> Option<Signal> {
        let c = self.counts(pos)?;
        let (count, indel) = c.top();
        let depth = c.depth();
        if count < MIN_READS || depth == 0 {
            return None;
        }
        let fraction = count as f32 / depth as f32;
        (fraction >= MIN_FRACTION).then_some(Signal {
            pos,
            fraction,
            indel,
        })
    }

    /// The part of `a..=b` the tally covers; `None` when it covers none.
    fn within(&self, a: u64, b: u64) -> Option<std::ops::RangeInclusive<u64>> {
        let last = self.start + (self.aligned.len() as u64).checked_sub(1)?;
        let (a, b) = (a.max(self.start), b.min(last));
        (a <= b).then_some(a..=b)
    }

    /// The strongest qualifying signal in `a..=b`, the first on a tie.
    pub fn strongest(&self, a: u64, b: u64) -> Option<Signal> {
        self.within(a, b)?.filter_map(|pos| self.signal(pos)).fold(
            None,
            |best: Option<Signal>, s| match best {
                Some(best) if best.fraction >= s.fraction => Some(best),
                _ => Some(s),
            },
        )
    }

    /// How many positions in `a..=b` have a qualifying signal.
    pub fn qualifying(&self, a: u64, b: u64) -> usize {
        self.within(a, b).map_or(0, |range| {
            range.filter(|&pos| self.signal(pos).is_some()).count()
        })
    }

    /// Mismatch and indel rates over `a..=b`; `None` with no aligned base.
    pub fn summary(&self, a: u64, b: u64) -> Option<Summary> {
        let (mut aligned, mut mismatches, mut indels) = (0u64, 0u64, 0u64);
        for i in self.within(a, b)?.filter_map(|pos| self.index(pos)) {
            aligned += u64::from(self.aligned[i]);
            mismatches += self.alt[i].iter().map(|&n| u64::from(n)).sum::<u64>();
            indels += u64::from(self.deletion_starts[i] + self.inserted[i]);
        }
        (aligned > 0).then(|| Summary {
            mismatch_rate: mismatches as f64 / aligned as f64,
            indels_per_kb: indels as f64 * 1000.0 / aligned as f64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bam::Read;

    const REF: RefSlice<'static> = RefSlice {
        start: 1,
        bases: &[b'A'; 400],
    };

    fn read(start: u64, cigar: &[(char, u32)], seq: &str, md: Option<&str>) -> Read {
        let span: u64 = cigar
            .iter()
            .filter(|(op, _)| matches!(op, 'M' | 'D' | 'N' | '=' | 'X'))
            .map(|&(_, n)| u64::from(n))
            .sum();
        Read {
            start,
            end: start + span - 1,
            cigar: cigar.to_vec(),
            seq: seq.as_bytes().to_vec(),
            md: md.map(str::to_string),
            ..Default::default()
        }
    }

    /// `len` aligned bases of `A` from `start`, with `base` at each of `at`.
    fn with(start: u64, len: u32, at: &[(u64, u8)]) -> Read {
        let mut seq = vec![b'A'; len as usize];
        for &(pos, base) in at {
            seq[(pos - start) as usize] = base;
        }
        read(
            start,
            &[('M', len)],
            std::str::from_utf8(&seq).unwrap(),
            None,
        )
    }

    /// `len` bases from `start` with position `gone` deleted.
    fn deleting(start: u64, len: u32, gone: u64) -> Read {
        let before = (gone - start) as u32;
        let after = len - before;
        read(
            start,
            &[('M', before), ('D', 1), ('M', after)],
            &"A".repeat(len as usize),
            None,
        )
    }

    fn reads(n: usize, f: impl Fn(usize) -> Read) -> Vec<Read> {
        (0..n).map(f).collect()
    }

    #[test]
    fn edits_from_a_reference_slice() {
        let r = read(11, &[('M', 5)], "AACAA", None);
        assert_eq!(edits(&r, Some(REF)), Some(vec![(13, Edit::Mismatch(b'C'))]));
    }

    #[test]
    fn edits_from_md_without_a_reference() {
        let r = read(11, &[('M', 5)], "AACAA", Some("2A2"));
        assert_eq!(edits(&r, None), Some(vec![(13, Edit::Mismatch(b'C'))]));
        let bare = read(11, &[('M', 5)], "AACAA", None);
        assert_eq!(edits(&bare, None), None);
    }

    #[test]
    fn edits_include_deletions_and_insertions() {
        let d = read(11, &[('M', 2), ('D', 2), ('M', 2)], "AAAA", Some("2^AA2"));
        assert_eq!(
            edits(&d, None),
            Some(vec![(13, Edit::Deleted), (14, Edit::Deleted)])
        );
        let i = read(11, &[('M', 2), ('I', 1), ('M', 2)], "AACAA", None);
        assert_eq!(edits(&i, Some(REF)), Some(vec![(13, Edit::Inserted)]));
    }

    #[test]
    fn n_bases_are_not_mismatches() {
        let r = read(11, &[('M', 5)], "AANAA", None);
        assert_eq!(edits(&r, Some(REF)), Some(vec![]));
    }

    #[test]
    fn tally_counts_alleles_by_base() {
        let rs = reads(10, |i| match i {
            0..4 => with(1, 5, &[(3, b'C')]),
            4..6 => with(1, 5, &[(3, b'G')]),
            _ => with(1, 5, &[]),
        });
        let t = Tally::of(&rs, Some(REF), 1, 5).unwrap();
        assert_eq!(
            t.counts(3),
            Some(Counts {
                reference: Some(b'A'),
                aligned: 10,
                alt: [0, 4, 2, 0],
                deleted: 0,
                inserted: 0,
            })
        );
        assert_eq!(
            t.strongest(1, 5),
            Some(Signal {
                pos: 3,
                fraction: 0.4,
                indel: false
            })
        );
    }

    #[test]
    fn floors_need_three_reads_and_fifteen_percent() {
        let c_at_3 = |n: usize, of: usize| {
            reads(of, move |i| {
                with(1, 5, if i < n { &[(3, b'C')] } else { &[] })
            })
        };
        let two = Tally::of(&c_at_3(2, 10), Some(REF), 1, 5).unwrap();
        assert_eq!(two.strongest(1, 5), None, "20%, but only 2 reads");
        let thin = Tally::of(&c_at_3(4, 30), Some(REF), 1, 5).unwrap();
        assert_eq!(thin.strongest(1, 5), None, "13%");
        let enough = Tally::of(&c_at_3(5, 30), Some(REF), 1, 5).unwrap();
        assert!(enough.strongest(1, 5).is_some(), "17%");
    }

    #[test]
    fn indel_wins_a_tie() {
        let rs = reads(10, |i| match i {
            0..4 => with(1, 5, &[(3, b'C')]),
            4..8 => deleting(1, 5, 3),
            _ => with(1, 5, &[]),
        });
        let t = Tally::of(&rs, Some(REF), 1, 10).unwrap();
        let s = t.strongest(1, 10).unwrap();
        assert_eq!((s.pos, s.indel), (3, true));
    }

    #[test]
    fn qualifying_counts_positions() {
        let rs = reads(10, |_| with(1, 5, &[(1, b'C'), (2, b'G'), (4, b'T')]));
        let t = Tally::of(&rs, Some(REF), 1, 5).unwrap();
        assert_eq!(t.qualifying(1, 5), 3);
        assert_eq!(t.qualifying(4, 5), 1);
    }

    #[test]
    fn summary_rates() {
        let rs = reads(10, |i| match i {
            0 => with(1, 10, &[(2, b'C'), (5, b'G')]),
            1 => deleting(1, 10, 6),
            _ => with(1, 10, &[]),
        });
        let t = Tally::of(&rs, Some(REF), 1, 20).unwrap();
        let s = t.summary(1, 20).unwrap();
        let aligned = 100.0;
        assert!((s.mismatch_rate - 2.0 / aligned).abs() < 1e-9, "{s:?}");
        assert!((s.indels_per_kb - 1000.0 / aligned).abs() < 1e-9, "{s:?}");
    }

    #[test]
    fn uncounted_reads_are_left_out() {
        let mut dup = with(1, 5, &[(3, b'C')]);
        dup.flags = 0x400;
        let plain = with(1, 5, &[]);
        let t = Tally::of(&[dup, plain], Some(REF), 1, 5).unwrap();
        assert_eq!(t.counts(3).unwrap().aligned, 1);
        assert_eq!(t.counts(3).unwrap().alt, [0; 4]);
    }

    #[test]
    fn tally_ignores_positions_outside_its_region() {
        let rs = reads(5, |_| deleting(1, 20, 9));
        let t = Tally::of(&rs, Some(REF), 5, 8).unwrap();
        assert_eq!(t.counts(4), None);
        assert_eq!(t.counts(9), None);
        assert_eq!(t.counts(5).unwrap().aligned, 5);
    }

    #[test]
    fn reads_without_seq_or_with_bad_md_add_nothing() {
        let no_seq = read(1, &[('M', 5)], "", Some("5"));
        let bad_md = read(1, &[('M', 2), ('D', 2), ('M', 2)], "AAAA", Some("5"));
        assert_eq!(edits(&no_seq, None), None);
        assert_eq!(edits(&bad_md, None), None);
        assert!(Tally::of(&[no_seq, bad_md], None, 1, 10).is_none());
    }

    #[test]
    fn reads_without_seq_add_nothing_with_a_reference_too() {
        let no_seq = read(1, &[('M', 5)], "", None);
        assert_eq!(edits(&no_seq, Some(REF)), None);
        let plain = with(1, 5, &[]);
        let t = Tally::of(&[no_seq, plain], Some(REF), 1, 5).unwrap();
        assert_eq!(t.counts(3).unwrap().aligned, 1);
    }

    #[test]
    fn equals_in_seq_matches_the_reference() {
        let r = read(11, &[('M', 5)], "AA=AC", None);
        assert_eq!(edits(&r, Some(REF)), Some(vec![(15, Edit::Mismatch(b'C'))]));
    }

    #[test]
    fn with_a_reference_a_window_without_reads_still_has_a_tally() {
        let t = Tally::of(&[], Some(REF), 1, 10).expect("a reference says what to compare with");
        assert_eq!(t.counts(5).unwrap().depth(), 0);
        assert_eq!(t.strongest(1, 10), None);
        assert_eq!(t.summary(1, 10), None);
    }

    #[test]
    fn queries_far_outside_the_tally_are_quick() {
        let rs = reads(10, |_| with(1, 5, &[(3, b'C')]));
        let t = Tally::of(&rs, Some(REF), 1, 10).unwrap();
        let timer = std::time::Instant::now();
        let far = 400_000_000;
        assert_eq!(t.strongest(1, far).map(|s| s.pos), Some(3));
        assert_eq!(t.qualifying(1, far), 1);
        assert_eq!(t.summary(1, far), t.summary(1, 10));
        assert_eq!(t.strongest(far, far + 10), None);
        assert!(timer.elapsed().as_millis() < 100, "{:?}", timer.elapsed());
    }

    #[test]
    #[ignore = "timing; run with --release"]
    fn tally_at_the_reads_limit_is_quick() {
        let len = 200_000u64;
        let rs: Vec<Read> = (0..100_000u64)
            .map(|i| {
                let start = 1 + (i * 7919) % (len - 150);
                with(start, 150, &[(start + 75, b'C')])
            })
            .collect();
        let bases = vec![b'A'; len as usize];
        let reference = Some(RefSlice {
            start: 1,
            bases: &bases,
        });
        let timer = std::time::Instant::now();
        let t = Tally::of(&rs, reference, 1, len);
        let tally_took = timer.elapsed();
        assert!(t.is_some());
        let timer = std::time::Instant::now();
        let all: Vec<_> = rs.iter().map(|r| edits(r, reference)).collect();
        let edits_took = timer.elapsed();
        assert_eq!(all.len(), rs.len());
        eprintln!("tally {tally_took:?}, edits {edits_took:?}");
        assert!(tally_took.as_millis() < 500, "tally took {tally_took:?}");
        assert!(edits_took.as_millis() < 500, "edits took {edits_took:?}");
    }
}
