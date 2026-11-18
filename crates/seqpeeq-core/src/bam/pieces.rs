//! A read broken into what the view draws: bases, deletions, skips,
//! insertions and clipped ends, each at its reference position.

use crate::bam::Read;

/// One drawable piece of a read, in the order the CIGAR gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// An M, = or X base; `None` when the record has no SEQ.
    Base {
        pos: u64,
        base: Option<u8>,
    },
    Deleted {
        pos: u64,
    },
    /// An N: the stretch `start..=end` the read skips.
    Skipped {
        start: u64,
        end: u64,
    },
    /// Bases inserted just before the aligned base at `before`.
    Inserted {
        before: u64,
        bases: Vec<u8>,
    },
    /// A soft-clipped base, placed where it would lie beyond the aligned
    /// part.
    Clipped {
        pos: u64,
        base: u8,
    },
}

/// What the MD tag says about the bases it covers, in order.
enum Md {
    Same(u64),
    Differs(u8),
    Deleted(Vec<u8>),
}

fn parse_md(md: &str) -> Option<Vec<Md>> {
    let mut out = Vec::new();
    let mut bytes = md.bytes().peekable();
    while let Some(&b) = bytes.peek() {
        if b.is_ascii_digit() {
            let mut n = 0u64;
            while let Some(&d) = bytes.peek().filter(|d| d.is_ascii_digit()) {
                n = n.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
                bytes.next();
            }
            if n > 0 {
                out.push(Md::Same(n));
            }
        } else if b == b'^' {
            bytes.next();
            let mut gone = Vec::new();
            while let Some(&c) = bytes.peek().filter(|c| c.is_ascii_alphabetic()) {
                gone.push(c.to_ascii_uppercase());
                bytes.next();
            }
            out.push(Md::Deleted(gone));
        } else if b.is_ascii_alphabetic() {
            out.push(Md::Differs(b.to_ascii_uppercase()));
            bytes.next();
        } else {
            return None;
        }
    }
    Some(out)
}

impl Read {
    /// The read as the view draws it. Clipped bases on the left that would
    /// fall before position 1 are left out, as are clipped bases of a record
    /// with no SEQ.
    pub fn pieces(&self) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut pos = self.start;
        let mut query = 0usize;
        let mut aligned = false;
        let base = |i: usize| self.seq.get(i).copied();
        for &(op, len) in &self.cigar {
            let n = len as usize;
            match op {
                'M' | '=' | 'X' => {
                    for i in 0..n {
                        out.push(Piece::Base {
                            pos: pos + i as u64,
                            base: base(query + i),
                        });
                    }
                    pos += u64::from(len);
                    query += n;
                    aligned = true;
                }
                'D' => {
                    for i in 0..u64::from(len) {
                        out.push(Piece::Deleted { pos: pos + i });
                    }
                    pos += u64::from(len);
                }
                'N' if len > 0 => {
                    out.push(Piece::Skipped {
                        start: pos,
                        end: pos + u64::from(len) - 1,
                    });
                    pos += u64::from(len);
                }
                'I' => {
                    let bases = self.seq.get(query..query + n).unwrap_or_default();
                    out.push(Piece::Inserted {
                        before: pos,
                        bases: bases.to_vec(),
                    });
                    query += n;
                }
                'S' => {
                    for i in 0..n {
                        let at = if aligned {
                            Some(pos + i as u64)
                        } else {
                            (pos + i as u64).checked_sub(n as u64).filter(|&p| p >= 1)
                        };
                        if let (Some(at), Some(b)) = (at, base(query + i)) {
                            out.push(Piece::Clipped { pos: at, base: b });
                        }
                    }
                    query += n;
                }
                _ => {}
            }
        }
        out
    }

    /// The reference base at each M, = or X and D position, from the MD
    /// tag and SEQ; `None` without both, or when they disagree with the
    /// CIGAR.
    pub fn md_reference(&self) -> Option<Vec<(u64, u8)>> {
        let md = parse_md(self.md.as_deref()?)?;
        if self.seq.is_empty() {
            return None;
        }
        let mut md = md.into_iter();
        let mut same = 0u64;
        let mut out = Vec::new();
        let mut pos = self.start;
        let mut query = 0usize;
        for &(op, len) in &self.cigar {
            match op {
                'M' | '=' | 'X' => {
                    for _ in 0..len {
                        let reference = if same > 0 {
                            same -= 1;
                            self.seq.get(query)?.to_ascii_uppercase()
                        } else {
                            match md.next()? {
                                Md::Same(n) => {
                                    same = n - 1;
                                    self.seq.get(query)?.to_ascii_uppercase()
                                }
                                Md::Differs(b) => b,
                                Md::Deleted(_) => return None,
                            }
                        };
                        out.push((pos, reference));
                        pos += 1;
                        query += 1;
                    }
                }
                'D' => {
                    if same > 0 {
                        return None;
                    }
                    let Md::Deleted(gone) = md.next()? else {
                        return None;
                    };
                    if gone.len() != len as usize {
                        return None;
                    }
                    for b in gone {
                        out.push((pos, b));
                        pos += 1;
                    }
                }
                'N' => pos += u64::from(len),
                'I' | 'S' => query += len as usize,
                _ => {}
            }
        }
        (same == 0 && md.next().is_none()).then_some(out)
    }

    /// Bases the read has (M, I, S, = and X), whether or not SEQ is stored.
    pub fn length(&self) -> u64 {
        self.cigar
            .iter()
            .filter(|(op, _)| matches!(op, 'M' | 'I' | 'S' | '=' | 'X'))
            .map(|&(_, n)| u64::from(n))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bam::testing::ref_bases;

    fn cigar(text: &str) -> Vec<(char, u32)> {
        let mut out = Vec::new();
        let mut n = 0;
        for c in text.chars() {
            match c.to_digit(10) {
                Some(d) => n = n * 10 + d,
                None => {
                    out.push((c, n));
                    n = 0;
                }
            }
        }
        out
    }

    fn read_md(ops: &str, start: u64, seq: &[u8], md: &str) -> Read {
        let cigar = cigar(ops);
        let span: u32 = cigar
            .iter()
            .filter(|(op, _)| matches!(op, 'M' | '=' | 'X' | 'D' | 'N'))
            .map(|(_, n)| n)
            .sum();
        Read {
            name: "x".to_string(),
            flags: 0,
            contig: 0,
            start,
            end: start + u64::from(span) - 1,
            mapq: 60,
            cigar,
            seq: seq.to_vec(),
            md: (!md.is_empty()).then(|| md.to_string()),
            mate: None,
            parts: Vec::new(),
            ..Read::default()
        }
    }

    fn read(ops: &str, start: u64, seq: &[u8]) -> Read {
        read_md(ops, start, seq, "")
    }

    fn r1() -> Read {
        let mut seq = ref_bases("c1", 101, 150).into_bytes();
        seq[9] = b'G';
        read_md("50M", 101, &seq, "9C40")
    }

    fn r2() -> Read {
        let seq = format!(
            "{}{}{}",
            ref_bases("c2", 51, 60),
            ref_bases("c1", 121, 150),
            ref_bases("c1", 156, 165)
        );
        read("10S30M5D10M", 121, seq.as_bytes())
    }

    #[test]
    fn pieces_follow_the_cigar_with_clips_placed_outside() {
        let p = r2().pieces();
        assert_eq!(
            p[0],
            Piece::Clipped {
                pos: 111,
                base: ref_bases("c2", 51, 51).as_bytes()[0]
            }
        );
        assert_eq!(
            p.iter()
                .filter(|x| matches!(x, Piece::Clipped { .. }))
                .count(),
            10
        );
        assert_eq!(
            p[10],
            Piece::Base {
                pos: 121,
                base: Some(b'A')
            }
        );
        assert_eq!(p[40], Piece::Deleted { pos: 151 });
        assert_eq!(
            p[45],
            Piece::Base {
                pos: 156,
                base: Some(ref_bases("c1", 156, 156).as_bytes()[0])
            }
        );
        assert_eq!(p.len(), 10 + 30 + 5 + 10);
    }

    #[test]
    fn insertions_skips_and_missing_seq() {
        let p = read("3M2I3M10N2M", 1, b"ACGTTACGAA").pieces();
        assert!(p.contains(&Piece::Inserted {
            before: 4,
            bases: b"TT".to_vec()
        }));
        assert!(p.contains(&Piece::Skipped { start: 7, end: 16 }));
        assert_eq!(
            p.last(),
            Some(&Piece::Base {
                pos: 18,
                base: Some(b'A')
            })
        );
        assert!(
            read("5M", 1, b"")
                .pieces()
                .iter()
                .all(|x| matches!(x, Piece::Base { base: None, .. }))
        );
        assert_eq!(read("3S2M", 2, b"AAACC").pieces().len(), 1 + 2);
        // A clip on the right goes after the last aligned base.
        assert_eq!(
            read("2M2S", 5, b"ACGT").pieces().last(),
            Some(&Piece::Clipped { pos: 8, base: b'T' })
        );
    }

    #[test]
    fn md_gives_the_reference_under_the_read() {
        let mut r1 = r1();
        let refs = r1.md_reference().unwrap();
        assert_eq!(refs.len(), 50);
        assert_eq!(refs[9], (110, b'C'));
        assert_eq!(refs[10], (111, r1.seq[10]));
        let d = read_md("5M2D5M", 101, b"AAAAACCCCC", "5^GT5")
            .md_reference()
            .unwrap();
        assert_eq!(&d[5..7], &[(106, b'G'), (107, b'T')]);
        assert_eq!(d[7], (108, b'C'));
        r1.md = None;
        assert_eq!(r1.md_reference(), None);
        assert_eq!(read_md("5M", 1, b"", "5").md_reference(), None, "no SEQ");
        assert_eq!(
            read_md("5M", 1, b"AAAAA", "9").md_reference(),
            None,
            "an MD longer than the CIGAR is not trusted"
        );
    }

    #[test]
    fn length_counts_query_bases() {
        assert_eq!(r2().length(), 50);
        assert_eq!(read("10M40H", 51, b"").length(), 10);
    }
}
