//! The read-as-axis view's model: every place a read and its mate
//! aligned, laid out along the read's own bases.

use std::collections::HashMap;

use seqpeeq_core::bam::{Read, parse_cigar, read_span, segment};

use crate::bam_view::zoom_levels;

const UNMAPPED: u16 = 0x4;
const REVERSE: u16 = 0x10;
const SECONDARY: u16 = 0x100;
const SUPPLEMENTARY: u16 = 0x800;

/// What kind of alignment a placement is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Supplementary,
    Secondary,
    /// An alternative hit from bwa's XA tag.
    Alternative,
    Unmapped,
}

impl Kind {
    /// The word the view shows.
    #[allow(dead_code)] // drawn in phase 5 task 7
    pub fn word(self) -> &'static str {
        match self {
            Kind::Primary => "primary",
            Kind::Supplementary => "supplementary",
            Kind::Secondary => "secondary",
            Kind::Alternative => "XA",
            Kind::Unmapped => "unmapped",
        }
    }
}

/// How a placement became known, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Found {
    /// A record in hand: the read, or one fetched through the index.
    Record,
    /// A record the name search found.
    Search,
    /// An entry of a record's SA tag.
    Sa,
    /// An entry of a record's XA tag.
    Xa,
}

/// One place a read aligned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub kind: Kind,
    pub found: Found,
    pub contig: usize,
    pub pos: u64,
    pub reverse: bool,
    pub cigar: Vec<(char, u32)>,
    /// `None` for an XA entry, which gives none.
    pub mapq: Option<u8>,
    pub nm: Option<u32>,
    pub score: Option<i64>,
    /// The bases of the read it covers, 1-based, in sequencing orientation;
    /// `None` when nothing is aligned.
    pub span: Option<(u64, u64)>,
}

/// One read of a pair (or the read, when it is not paired) and its places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// 1 or 2 for a read of a pair, else 0.
    pub number: u8,
    /// The read's length with clips, 0 when nothing gives one.
    pub length: u64,
    /// Primary first, then by where on the read they start.
    pub placements: Vec<Placement>,
    /// The primary's XS tag.
    pub suboptimal: Option<i64>,
    /// XA entries left out.
    pub alts_skipped: usize,
}

impl Segment {
    /// Stretches of the read no placement covers.
    #[allow(dead_code)] // drawn in phase 5 task 7
    pub fn uncovered(&self) -> Vec<(u64, u64)> {
        let mut spans: Vec<(u64, u64)> = self.placements.iter().filter_map(|p| p.span).collect();
        spans.sort_unstable();
        let mut out = Vec::new();
        let mut next = 1u64;
        for (start, end) in spans {
            if start > next {
                out.push((next, start - 1));
            }
            next = next.max(end + 1);
        }
        if self.length >= next {
            out.push((next, self.length));
        }
        out
    }
}

/// The bases a CIGAR says the read has, hard clips included.
fn query_length(cigar: &[(char, u32)]) -> u64 {
    cigar
        .iter()
        .filter(|(op, _)| matches!(op, 'M' | 'I' | 'S' | '=' | 'X' | 'H'))
        .map(|&(_, n)| u64::from(n))
        .sum()
}

/// What one segment is gathering.
#[derive(Default)]
struct Gathering {
    length: u64,
    placements: HashMap<(usize, u64, bool), Placement>,
    suboptimal: Option<i64>,
    alts_skipped: usize,
}

impl Gathering {
    /// Keep `p` unless a placement at the same place was found a better way.
    fn offer(&mut self, p: Placement) {
        self.length = self.length.max(query_length(&p.cigar));
        let key = (p.contig, p.pos, p.reverse);
        match self.placements.get(&key) {
            Some(old) if old.found <= p.found => {}
            _ => {
                self.placements.insert(key, p);
            }
        }
    }
}

/// Every placement the records name: the records themselves, their SA
/// entries and their XA entries, one per place, grouped by segment.
pub fn assemble(records: &[&Read], found_by_search: &[&Read]) -> Vec<Segment> {
    let mut segments: [Gathering; 3] = Default::default();
    let tagged = records
        .iter()
        .map(|r| (*r, Found::Record))
        .chain(found_by_search.iter().map(|r| (*r, Found::Search)));
    for (read, found) in tagged {
        let seg = &mut segments[usize::from(segment(read.flags))];
        let flags = read.flags;
        let reverse = flags & REVERSE != 0;
        let kind = if flags & UNMAPPED != 0 {
            Kind::Unmapped
        } else if flags & SECONDARY != 0 {
            Kind::Secondary
        } else if flags & SUPPLEMENTARY != 0 {
            Kind::Supplementary
        } else {
            Kind::Primary
        };
        seg.length = seg.length.max(read.seq.len() as u64);
        if kind == Kind::Primary {
            seg.suboptimal = read.suboptimal;
            seg.alts_skipped = read.alts_skipped;
        }
        let span = if kind == Kind::Unmapped {
            None
        } else {
            read_span(&read.cigar, reverse).map(|(a, b, _)| (a, b))
        };
        seg.offer(Placement {
            kind,
            found,
            contig: read.contig,
            pos: read.start,
            reverse,
            cigar: read.cigar.clone(),
            mapq: Some(read.mapq),
            nm: read.nm,
            score: read.score,
            span,
        });
        // A supplementary's SA tag names the primary first.
        for (i, part) in read.parts.iter().enumerate() {
            let Some(cigar) = parse_cigar(&part.cigar) else {
                continue;
            };
            let span = read_span(&cigar, part.place.reverse).map(|(a, b, _)| (a, b));
            let kind = if i == 0 && kind == Kind::Supplementary {
                Kind::Primary
            } else {
                Kind::Supplementary
            };
            seg.offer(Placement {
                kind,
                found: Found::Sa,
                contig: part.place.contig,
                pos: part.place.pos,
                reverse: part.place.reverse,
                cigar,
                mapq: Some(part.mapq),
                nm: part.nm,
                score: None,
                span,
            });
        }
        for alt in &read.alts {
            let Some(cigar) = parse_cigar(&alt.cigar) else {
                continue;
            };
            let span = read_span(&cigar, alt.place.reverse).map(|(a, b, _)| (a, b));
            seg.offer(Placement {
                kind: Kind::Alternative,
                found: Found::Xa,
                contig: alt.place.contig,
                pos: alt.place.pos,
                reverse: alt.place.reverse,
                cigar,
                mapq: None,
                nm: alt.nm,
                score: None,
                span,
            });
        }
    }
    segments
        .into_iter()
        .enumerate()
        .filter(|(_, g)| !g.placements.is_empty())
        .map(|(number, g)| {
            let mut placements: Vec<Placement> = g.placements.into_values().collect();
            placements.sort_by_key(|p| {
                (
                    p.kind != Kind::Primary,
                    p.span.map_or(u64::MAX, |(a, _)| a),
                    p.contig,
                    p.pos,
                    p.reverse,
                )
            });
            Segment {
                number: number as u8,
                length: g.length,
                placements,
                suboptimal: g.suboptimal,
                alts_skipped: g.alts_skipped,
            }
        })
        .collect()
}

/// The read-as-axis view: the read's bases across, one bar per placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisView {
    pub name: String,
    pub segments: Vec<Segment>,
    /// Read bases per column.
    pub per_col: u64,
    /// The read base at the first column.
    pub left: u64,
    /// The picked placement, in drawing order.
    pub picked: usize,
    /// The first placement row shown.
    pub top: usize,
    /// Columns for the bars, and rows for placements; set when drawn.
    pub width: usize,
    pub height: usize,
    /// Whether the BAM has been searched for this name.
    pub searched: bool,
    /// The records in hand: the read, and those fetched through the index.
    pub records: Vec<Read>,
    /// Where `z` came from, to go back.
    before_whole: Option<(u64, u64)>,
}

impl AxisView {
    /// The whole read in `width` columns, the first placement picked.
    pub fn new(name: &str, segments: Vec<Segment>, width: usize) -> AxisView {
        let mut v = AxisView {
            name: name.to_string(),
            segments,
            per_col: 1,
            left: 1,
            picked: 0,
            top: 0,
            width: width.max(1),
            height: 0,
            searched: false,
            records: Vec::new(),
            before_whole: None,
        };
        v.per_col = v.whole();
        v
    }

    /// The placements again, from the records in hand and `found` by the
    /// search, keeping the zoom unless it showed the whole read.
    pub fn rebuild(&mut self, found: &[&Read]) {
        let whole = self.per_col == self.whole();
        let records: Vec<&Read> = self.records.iter().collect();
        self.segments = assemble(&records, found);
        if whole {
            self.per_col = self.whole();
        }
        self.clamp();
        self.picked = self.picked.min(self.placements().len().saturating_sub(1));
    }

    /// The longest segment's length, at least 1.
    pub fn length(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| s.length)
            .max()
            .unwrap_or(0)
            .max(1)
    }

    fn whole(&self) -> u64 {
        *zoom_levels(self.length(), self.width)
            .last()
            .expect("a level")
    }

    /// Every placement with its segment's number, in drawing order.
    pub fn placements(&self) -> Vec<(u8, &Placement)> {
        self.segments
            .iter()
            .flat_map(|s| s.placements.iter().map(move |p| (s.number, p)))
            .collect()
    }

    /// The picked placement.
    pub fn current(&self) -> Option<(u8, &Placement)> {
        self.placements().get(self.picked).copied()
    }

    /// Pick the placement `by` rows on, stopping at either end.
    pub fn pick(&mut self, by: isize) {
        let last = self.placements().len().saturating_sub(1);
        self.picked = self.picked.saturating_add_signed(by).min(last);
        if self.picked < self.top {
            self.top = self.picked;
        } else if self.height > 0 && self.picked >= self.top + self.height {
            self.top = self.picked + 1 - self.height;
        }
    }

    /// `+` zooms in, `-` out, `z` shows the whole read or goes back.
    pub fn zoom(&mut self, code: char) {
        let levels = zoom_levels(self.length(), self.width);
        let now = levels
            .iter()
            .position(|&l| l >= self.per_col)
            .unwrap_or(levels.len() - 1);
        match code {
            '+' => self.per_col = levels[now.saturating_sub(1)],
            '-' => self.per_col = levels[(now + 1).min(levels.len() - 1)],
            'z' => {
                let whole = *levels.last().expect("a level");
                match self.before_whole.take() {
                    Some((per_col, left)) if self.per_col == whole && self.left == 1 => {
                        self.per_col = per_col;
                        self.left = left;
                    }
                    _ => {
                        self.before_whole = Some((self.per_col, self.left));
                        self.per_col = whole;
                        self.left = 1;
                    }
                }
                self.clamp();
                return;
            }
            _ => return,
        }
        self.before_whole = None;
        self.clamp();
    }

    /// Move `cols` columns to the right (left when negative).
    pub fn scroll(&mut self, cols: i64) {
        let by = cols.saturating_mul(self.per_col as i64);
        self.left = self.left.saturating_add_signed(by);
        self.clamp();
    }

    /// Keep the read's end from leaving the last column empty.
    fn clamp(&mut self) {
        let shown = (self.width as u64).saturating_mul(self.per_col);
        let last = self.length().saturating_sub(shown) + 1;
        self.left = self.left.clamp(1, last.max(1));
    }

    /// The column showing read base `pos`, if one does.
    #[allow(dead_code)] // drawn in phase 5 task 7
    pub fn col_of(&self, pos: u64) -> Option<usize> {
        let col = pos.checked_sub(self.left)? / self.per_col;
        (col < self.width as u64).then_some(col as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqpeeq_core::bam::{BamSource, Read};
    use seqpeeq_core::{Command, Session};
    use std::collections::HashSet;

    /// The `multi` test BAM's source, and every record in it.
    pub(crate) fn multi() -> (BamSource, Vec<Read>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = seqpeeq_core::bam::testing::multi(dir.path());
        let mut session = Session::new(None);
        session.execute(Command::Open { path, kind: None }).unwrap();
        let source = session.datasets()[0].bam().unwrap().source().clone();
        let names: HashSet<String> = ["x1", "y1", "z1", "p1", "u1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let all = source
            .search_names(&names, &seqpeeq_core::Progress::new())
            .unwrap();
        (source, all, dir)
    }

    fn rec<'a>(all: &'a [Read], name: &str, flags: u16, start: u64) -> &'a Read {
        all.iter()
            .find(|r| r.name == name && r.flags == flags && r.start == start)
            .unwrap()
    }

    /// Each placement as (kind, found, contig, pos, reverse, span).
    type Shape = (Kind, Found, usize, u64, bool, Option<(u64, u64)>);

    fn shape(s: &Segment) -> Vec<Shape> {
        s.placements
            .iter()
            .map(|p| (p.kind, p.found, p.contig, p.pos, p.reverse, p.span))
            .collect()
    }

    use Found::{Record, Sa, Search, Xa};
    use Kind::{Alternative, Primary, Secondary, Supplementary, Unmapped};

    #[test]
    fn a_bwa_pair_shows_its_xa_alternatives_under_read_1() {
        let (_, all, _dir) = multi();
        let segs = assemble(&[rec(&all, "x1", 99, 101), rec(&all, "x1", 147, 401)], &[]);
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].number, segs[0].length), (1, 50));
        assert_eq!(
            shape(&segs[0]),
            [
                (Primary, Record, 0, 101, false, Some((1, 50))),
                (Alternative, Xa, 1, 301, false, Some((1, 50))),
                (Alternative, Xa, 2, 501, true, Some((1, 50))),
            ]
        );
        assert_eq!(segs[0].placements[1].nm, Some(1));
        assert_eq!(segs[0].placements[2].nm, Some(2));
        assert_eq!(segs[0].placements[1].mapq, None);
        assert_eq!(segs[0].suboptimal, Some(50));
        assert_eq!((segs[1].number, segs[1].length), (2, 50));
        assert_eq!(
            shape(&segs[1]),
            [(Primary, Record, 0, 401, true, Some((1, 50)))]
        );
    }

    #[test]
    fn a_split_read_lies_along_its_own_bases_once() {
        let (_, all, _dir) = multi();
        let primary = rec(&all, "z1", 0, 1001);
        let supplementary = rec(&all, "z1", 2048, 1501);
        let segs = assemble(&[primary, supplementary], &[]);
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].number, segs[0].length), (0, 100));
        assert_eq!(
            shape(&segs[0]),
            [
                (Primary, Record, 0, 1001, false, Some((1, 60))),
                (Supplementary, Record, 1, 1501, false, Some((61, 100))),
            ]
        );
        assert_eq!(segs[0].uncovered(), []);
        let segs = assemble(&[primary], &[]);
        assert_eq!(
            shape(&segs[0]),
            [
                (Primary, Record, 0, 1001, false, Some((1, 60))),
                (Supplementary, Sa, 1, 1501, false, Some((61, 100))),
            ]
        );
        assert_eq!(
            (segs[0].placements[1].mapq, segs[0].placements[1].nm),
            (Some(60), Some(1))
        );
        let segs = assemble(&[supplementary], &[]);
        assert_eq!(
            shape(&segs[0]),
            [
                (Primary, Sa, 0, 1001, false, Some((1, 60))),
                (Supplementary, Record, 1, 1501, false, Some((61, 100))),
            ],
            "an SA entry from a supplementary names the primary first"
        );
    }

    #[test]
    fn search_results_add_secondaries() {
        let (_, all, _dir) = multi();
        let found = [rec(&all, "y1", 256, 801), rec(&all, "y1", 256, 101)];
        let segs = assemble(&[rec(&all, "y1", 0, 201)], &found);
        assert_eq!(
            shape(&segs[0]),
            [
                (Primary, Record, 0, 201, false, Some((1, 100))),
                (Secondary, Search, 1, 801, false, Some((1, 100))),
                (Secondary, Search, 2, 101, false, Some((1, 100))),
            ]
        );
    }

    #[test]
    fn an_unmapped_mate_is_an_axis_with_no_bars() {
        let (_, all, _dir) = multi();
        let r1 = rec(&all, "u1", 73, 901);
        let r2 = all
            .iter()
            .find(|r| r.name == "u1" && r.flags == 133)
            .unwrap();
        let segs = assemble(&[r1], &[r1, r2]);
        assert_eq!(segs.len(), 2);
        assert_eq!(
            shape(&segs[0]),
            [(Primary, Record, 2, 901, false, Some((1, 50)))]
        );
        assert_eq!(segs[1].number, 2);
        assert_eq!(segs[1].length, 0);
        assert_eq!(segs[1].placements.len(), 1);
        assert_eq!(
            (segs[1].placements[0].kind, segs[1].placements[0].span),
            (Unmapped, None)
        );
    }

    fn read(cigar: &str, flags: u16, contig: usize, start: u64) -> Read {
        Read {
            name: "q".to_string(),
            flags,
            contig,
            start,
            end: start,
            mapq: 0,
            cigar: seqpeeq_core::bam::parse_cigar(cigar).unwrap(),
            ..Read::default()
        }
    }

    #[test]
    fn unaligned_stretches_are_listed() {
        let segs = assemble(&[&read("10S80M10S", 0, 0, 1)], &[]);
        assert_eq!(segs[0].uncovered(), [(1, 10), (91, 100)]);
    }

    #[test]
    fn hundreds_of_placements_assemble() {
        let primary = read("100M", 0, 0, 1000);
        let others: Vec<Read> = (1..=500).map(|pos| read("100M", 256, 0, pos)).collect();
        let refs: Vec<&Read> = others.iter().collect();
        let start = std::time::Instant::now();
        let segs = assemble(&[&primary], &refs);
        assert!(start.elapsed().as_millis() < 50, "{:?}", start.elapsed());
        assert_eq!(segs[0].placements.len(), 501);
        assert_eq!(segs[0].placements[0].kind, Primary);
    }

    #[test]
    fn the_axis_zooms_and_picks() {
        let (_, all, _dir) = multi();
        let segs = assemble(
            &[rec(&all, "z1", 0, 1001), rec(&all, "z1", 2048, 1501)],
            &[],
        );
        let mut v = AxisView::new("z1", segs, 50);
        assert_eq!((v.per_col, v.left), (2, 1));
        v.pick(1);
        assert_eq!(v.picked, 1);
        v.pick(5);
        assert_eq!(v.picked, 1, "stays on the last");
        v.pick(-9);
        assert_eq!(v.picked, 0);
        v.zoom('+');
        assert_eq!(v.per_col, 1);
        v.scroll(10);
        assert_eq!(v.left, 11);
        v.scroll(1000);
        assert_eq!(v.left, 51, "no further than the read's end");
        assert_eq!(v.col_of(51), Some(0));
        assert_eq!(v.col_of(50), None);
        v.zoom('z');
        assert_eq!((v.per_col, v.left), (2, 1));
        v.zoom('z');
        assert_eq!((v.per_col, v.left), (1, 51), "and back");
    }
}
