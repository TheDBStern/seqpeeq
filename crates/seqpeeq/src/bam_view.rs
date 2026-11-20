//! Where the user is in each open BAM, and the rules for what the view
//! shows: zoom levels, positions and columns, packing, and how a read is
//! described and coloured. Nothing here reads the BAM.

use crate::elsewhere::ElsewhereList;
use crate::read_axis::AxisView;
use std::cmp::{Ordering, Reverse};

use seqpeeq_core::bam::{Contig, Place, Read, Region, Window};

use crate::line::LineEdit;

/// Views at most this wide, in bases, show their reads.
pub const READS_WIDTH: u64 = 200_000;
/// Views with more reads than this show coverage only.
pub const READS_LIMIT: usize = 100_000;
/// The most rows of reads drawn.
pub const MAX_ROWS: usize = 1_000;
/// Bins in a contig's coverage pass (fewer for a shorter contig).
pub const PASS_BINS: u64 = 4_096;

/// What to read for a view: a region, bases per bin, and whether to read
/// the reads too.
pub type Want = (Region, u64, bool);

/// Whether what `have` read serves a view showing `shown` that wants
/// `want`.
pub fn serves(have: &Want, shown: &Region, want: &Want) -> bool {
    let (region, bin, reads) = have;
    region.contig == shown.contig
        && region.start <= shown.start
        && region.end >= shown.end
        && *bin == want.1
        && *reads == want.2
}

/// The bases each bin of `length`'s coverage pass covers, at least 1.
pub fn pass_bin(length: u64) -> u64 {
    length.div_ceil(PASS_BINS.min(length).max(1)).max(1)
}

/// A read, named well enough to find it again after the reads are read
/// afresh.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReadId {
    pub name: String,
    pub flags: u16,
    pub contig: usize,
    pub start: u64,
    /// The last reference position aligned.
    pub end: u64,
}

impl ReadId {
    pub fn of(read: &Read) -> ReadId {
        ReadId {
            name: read.name.clone(),
            flags: read.flags,
            contig: read.contig,
            start: read.start,
            end: read.end,
        }
    }
}

/// What a read's colour says about it; the first that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// It has a part elsewhere (an SA tag).
    Split,
    /// Its mate is on another contig.
    MateElsewhere,
    /// Paired, but not marked as a proper pair.
    Improper,
    Plain,
}

const PAIRED: u16 = 0x1;
const PROPER: u16 = 0x2;

pub fn read_kind(read: &Read) -> ReadKind {
    if !read.parts.is_empty() {
        ReadKind::Split
    } else if read.mate.is_some_and(|m| m.contig != read.contig) {
        ReadKind::MateElsewhere
    } else if read.flags & PAIRED != 0 && read.flags & PROPER == 0 {
        ReadKind::Improper
    } else {
        ReadKind::Plain
    }
}

/// Bases per column from one base up: 1, 2, 5, 10, 20, 50, … ending at the
/// first that shows all of `length` in `width` columns.
pub fn zoom_levels(length: u64, width: usize) -> Vec<u64> {
    let width = width.max(1) as u64;
    let mut levels = Vec::new();
    let mut decade = 1u64;
    'out: loop {
        for step in [1, 2, 5] {
            let level = decade.saturating_mul(step);
            levels.push(level);
            if level.saturating_mul(width) >= length || level == u64::MAX {
                break 'out;
            }
        }
        decade = decade.saturating_mul(10);
    }
    levels
}

/// Reads stacked into rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Packed {
    /// Each row's reads, as indexes, left to right.
    pub rows: Vec<Vec<usize>>,
    /// Reads that found no row.
    pub not_drawn: usize,
}

/// Stack `spans` (sorted by start) into rows, first fit: each goes into the
/// first row whose last read ends more than `gap` before it starts.
pub fn pack(spans: &[(u64, u64)], gap: u64, max_rows: usize) -> Packed {
    let mut packed = Packed::default();
    let mut ends: Vec<u64> = Vec::new();
    for (i, &(start, end)) in spans.iter().enumerate() {
        match ends.iter().position(|&e| start > e.saturating_add(gap)) {
            Some(row) => {
                packed.rows[row].push(i);
                ends[row] = end;
            }
            None if ends.len() < max_rows => {
                packed.rows.push(vec![i]);
                ends.push(end);
            }
            None => packed.not_drawn += 1,
        }
    }
    packed
}

fn place_text(place: &Place, contigs: &[Contig]) -> String {
    let name = contigs.get(place.contig).map_or("?", |c| c.name.as_str());
    format!("{name}:{} ({})", place.pos, strand(place.reverse))
}

fn strand(reverse: bool) -> &'static str {
    if reverse { "−" } else { "+" }
}

/// A CIGAR as text; one over 40 characters keeps its first and last four
/// operations.
pub fn cigar_text(cigar: &[(char, u32)]) -> String {
    let op = |&(op, n): &(char, u32)| format!("{n}{op}");
    let whole: String = cigar.iter().map(op).collect();
    if whole.len() <= 40 || cigar.len() <= 8 {
        return whole;
    }
    let head: String = cigar[..4].iter().map(op).collect();
    let tail: String = cigar[cigar.len() - 4..].iter().map(op).collect();
    format!("{head} … {tail}")
}

/// Inserted bases shown in the details line; more are cut with `…`.
const INSERTED_SHOWN: usize = 20;

/// The details line for one read, with the bases inserted just before
/// `cursor` when there are some.
pub fn details(read: &Read, contigs: &[Contig], cursor: u64) -> String {
    let name = contigs.get(read.contig).map_or("?", |c| c.name.as_str());
    let mut parts = vec![
        read.name.clone(),
        format!(
            "{name}:{}-{} ({})",
            read.start,
            read.end,
            strand(read.reverse())
        ),
    ];
    let inserted = read.pieces().into_iter().find_map(|piece| match piece {
        seqpeeq_core::bam::Piece::Inserted { before, bases } if before == cursor => Some(bases),
        _ => None,
    });
    if let Some(bases) = inserted {
        let mut shown =
            String::from_utf8_lossy(&bases[..bases.len().min(INSERTED_SHOWN)]).into_owned();
        if bases.len() > INSERTED_SHOWN {
            shown.push('…');
        }
        parts.push(format!("ins {} bp {shown} before {cursor}", bases.len()));
    }
    parts.extend([
        format!("MAPQ {}", read.mapq),
        format!("{} bp", read.length()),
        cigar_text(&read.cigar),
    ]);
    if let Some(mate) = &read.mate {
        parts.push(format!("mate {}", place_text(mate, contigs)));
    }
    for part in &read.parts {
        parts.push(format!(
            "part {} {}",
            place_text(&part.place, contigs),
            part.cigar
        ));
    }
    parts.join(" · ")
}

/// How the contig list is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContigSort {
    #[default]
    Name,
    /// Most mapped reads first.
    Reads,
}

/// `c`: the contigs of a BAM to pick one from.
#[derive(Debug, Clone)]
pub struct ContigList {
    /// The id of the dataset whose contigs these are.
    pub dataset: u64,
    pub filter: LineEdit,
    pub sort: ContigSort,
    /// Show contigs with no reads too.
    pub all: bool,
    /// The row picked, among those shown.
    pub cursor: usize,
    /// The first row shown.
    pub top: usize,
    /// How many contigs have no reads.
    pub hidden: usize,
    /// Opened by Tab in the `g` line: what that line held, to go back to
    /// with the contig picked (Enter) or as it was (Esc).
    pub back_to: Option<String>,
    by_name: Vec<usize>,
    by_reads: Vec<usize>,
}

/// Names compared with runs of digits as numbers: chr2 before chr10.
fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let n = x.iter().take_while(|b| b.is_ascii_digit()).count();
                let m = y.iter().take_while(|b| b.is_ascii_digit()).count();
                let trim = |s: &[u8]| -> usize { s.iter().take_while(|&&b| b == b'0').count() };
                let (p, q) = (&x[trim(&x[..n])..n], &y[trim(&y[..m])..m]);
                let order = p.len().cmp(&q.len()).then(p.cmp(q)).then(n.cmp(&m));
                if order != Ordering::Equal {
                    return order;
                }
                (x, y) = (&x[n..], &y[m..]);
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(d);
                }
                (x, y) = (&x[1..], &y[1..]);
            }
        }
    }
}

/// Whether `name` holds `needle`, which is in lower case, in any case.
pub(crate) fn holds(name: &str, needle: &str) -> bool {
    if name.is_ascii() && needle.is_ascii() {
        let (name, needle) = (name.as_bytes(), needle.as_bytes());
        return name.len() >= needle.len()
            && name
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle));
    }
    name.to_lowercase().contains(needle)
}

impl ContigList {
    /// A list of `contigs` of dataset `dataset`, sorted both ways once.
    pub fn new(dataset: u64, contigs: &[Contig]) -> ContigList {
        let mut by_name: Vec<usize> = (0..contigs.len()).collect();
        by_name.sort_by(|&a, &b| natural(&contigs[a].name, &contigs[b].name));
        let mut by_reads = by_name.clone();
        // Stable, so ties stay in name order; unknown counts go last.
        by_reads.sort_by_key(|&i| Reverse(contigs[i].mapped.map_or(0, |m| m + 1)));
        ContigList {
            dataset,
            filter: LineEdit::default(),
            sort: ContigSort::Name,
            all: false,
            cursor: 0,
            top: 0,
            hidden: contigs.iter().filter(|c| c.mapped == Some(0)).count(),
            back_to: None,
            by_name,
            by_reads,
        }
    }

    /// The contigs shown, as indexes, in order.
    pub fn rows(&self, contigs: &[Contig]) -> Vec<usize> {
        let order = match self.sort {
            ContigSort::Name => &self.by_name,
            ContigSort::Reads => &self.by_reads,
        };
        let needle = self.filter.text().to_lowercase();
        order
            .iter()
            .copied()
            .filter(|&i| self.all || contigs[i].mapped != Some(0))
            .filter(|&i| needle.is_empty() || holds(&contigs[i].name, &needle))
            .collect()
    }
}

/// What a window read in the background brought back.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub window: Window,
    /// Whether reads were asked for (they are `None` past the limit).
    pub asked_reads: bool,
    /// The reference across the window's region, when one is attached and
    /// reads were read. `None` too when the reference lacks the contig.
    pub reference: Option<Vec<u8>>,
    /// Whether a reference was attached when it was read, found or not.
    pub with_reference: bool,
}

/// A view opened over the reference view, drawn in its place.
#[derive(Debug, Clone)]
pub enum Over {
    /// `A`: the reads in view aligning elsewhere.
    List(ElsewhereList),
    /// `a`: one read's placements along its own bases.
    Axis(AxisView),
}

/// The reference view as `g` from the axis view left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parked {
    pub contig: usize,
    pub left: u64,
    pub per_col: u64,
    pub cursor: u64,
    pub row: usize,
    pub top: usize,
    pub before_whole: Option<(u64, u64)>,
    /// How long `back` was.
    pub back: usize,
}

/// Where a view is, as the BAMs of a stack share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackPlace {
    pub contig: usize,
    pub left: u64,
    pub per_col: u64,
    pub cursor: u64,
    pub before_whole: Option<(u64, u64)>,
    pub back: Vec<(usize, u64)>,
}

/// Where the user is in one BAM.
#[derive(Debug, Clone)]
pub struct BamView {
    pub contig: usize,
    /// The first position shown, at the left edge; `left - 1` is a whole
    /// number of columns.
    pub left: u64,
    /// Bases per column.
    pub per_col: u64,
    pub cursor: u64,
    /// The row of reads the cursor is on, counting from the first row.
    pub row: usize,
    /// The first row of reads shown.
    pub top: usize,
    /// Columns across.
    pub width: usize,
    /// Rows of reads shown.
    pub height: usize,
    /// `F`: show reads that do not count, faded.
    pub show_filtered: bool,
    /// `S`: hide clipped ends.
    pub hide_clips: bool,
    pub selected: Vec<ReadId>,
    /// A finished stretch of reference on this contig, `start <= end`.
    pub marked: Option<(u64, u64)>,
    /// The end a first `v` set, waiting for the second.
    pub marking: Option<u64>,
    /// Where each jump started, for `'`.
    pub back: Vec<(usize, u64)>,
    /// A read to put the cursor's row on once it is loaded: its name and
    /// start (for `m` and `s`).
    pub focus: Option<(String, u64)>,
    /// Where `z` goes back to: bases per column and left edge.
    pub before_whole: Option<(u64, u64)>,
    pub loaded: Option<Loaded>,
    /// Bumped whenever what should be read changes, so that a window read
    /// for an older view is dropped.
    pub generation: u64,
    /// The loaded reads that are shown, stacked; indexes into its reads.
    pub packed: Packed,
    /// Views opened over this one, the last on top.
    pub over: Vec<Over>,
    /// Views put aside by `g` from the axis view, for `'` to bring back,
    /// with the reference view as it was then.
    pub parked: Vec<Over>,
    pub parked_from: Option<Parked>,
}

impl BamView {
    /// The whole of `contig`, cursor at its start.
    pub fn new(contig: usize, length: u64, width: usize) -> BamView {
        BamView {
            contig,
            left: 1,
            per_col: *zoom_levels(length, width)
                .last()
                .expect("at least one level"),
            cursor: 1,
            row: 0,
            top: 0,
            width,
            height: 0,
            show_filtered: false,
            hide_clips: false,
            selected: Vec::new(),
            marked: None,
            marking: None,
            back: Vec::new(),
            focus: None,
            before_whole: None,
            loaded: None,
            generation: 0,
            packed: Packed::default(),
            over: Vec::new(),
            parked: Vec::new(),
            parked_from: None,
        }
    }

    pub fn cols(&self) -> u64 {
        self.width.max(1) as u64
    }

    /// The column showing `pos`, if one does.
    pub fn col_of(&self, pos: u64) -> Option<usize> {
        let col = pos.checked_sub(self.left)? / self.per_col;
        (col < self.cols()).then_some(col as usize)
    }

    /// The first and last positions column `col` covers.
    pub fn span_of(&self, col: usize) -> (u64, u64) {
        let a = self.left + col as u64 * self.per_col;
        (a, a + self.per_col - 1)
    }

    /// The last position shown.
    pub fn end(&self, length: u64) -> u64 {
        (self.left + self.cols() * self.per_col - 1).min(length)
    }

    /// `left` moved down to a column boundary, and no further right than
    /// needed to show the contig's end.
    fn place_left(&mut self, left: u64, length: u64) {
        let align = |p: u64| (p.max(1) - 1) / self.per_col * self.per_col + 1;
        let shown = self.cols() * self.per_col;
        // The leftmost edge that still shows the contig's end.
        let most = if length > shown {
            align(length - shown + self.per_col)
        } else {
            1
        };
        self.left = align(left).min(most).max(1);
    }

    /// Scroll just enough to show the cursor.
    pub fn reveal(&mut self, length: u64) {
        if self.cursor < self.left {
            self.place_left(self.cursor, length);
        } else if self.col_of(self.cursor).is_none() {
            let back = (self.cols() - 1) * self.per_col;
            self.place_left(self.cursor.saturating_sub(back), length);
        }
    }

    /// Zoom in (`step` < 0) or out, keeping the cursor in its column.
    pub fn zoom(&mut self, step: isize, length: u64) {
        let levels = zoom_levels(length, self.width);
        let now = levels
            .iter()
            .position(|&l| l >= self.per_col)
            .unwrap_or(levels.len() - 1);
        let next = now.saturating_add_signed(step).min(levels.len() - 1);
        let col = match self.col_of(self.cursor) {
            Some(col) => col as u64,
            None => self.cols() / 2,
        };
        self.per_col = levels[next];
        self.place_left(self.cursor.saturating_sub(col * self.per_col), length);
        self.reveal(length);
        self.before_whole = None;
    }

    /// `z`: the whole contig, or back to where it was.
    pub fn toggle_whole(&mut self, length: u64) {
        let whole = *zoom_levels(length, self.width).last().expect("a level");
        match self.before_whole.take() {
            Some((per_col, left)) if self.per_col == whole && self.left == 1 => {
                self.per_col = per_col;
                self.place_left(left, length);
            }
            _ => {
                self.before_whole = Some((self.per_col, self.left));
                self.per_col = whole;
                self.left = 1;
            }
        }
    }

    /// Move the cursor `by` columns.
    pub fn step(&mut self, by: i64, length: u64) {
        let moved = by.saturating_mul(self.per_col as i64);
        self.cursor = self
            .cursor
            .saturating_add_signed(moved)
            .clamp(1, length.max(1));
        self.reveal(length);
    }

    /// Put the cursor at `pos` on `contig`, remembering where it was for
    /// `'`, and centre it if it is off screen.
    pub fn go(&mut self, contig: usize, pos: u64, length: u64) {
        self.back.push((self.contig, self.cursor));
        self.place(contig, pos, length);
    }

    /// Where this view is, for the other BAMs of its stack to follow.
    pub fn place_now(&self) -> StackPlace {
        StackPlace {
            contig: self.contig,
            left: self.left,
            per_col: self.per_col,
            cursor: self.cursor,
            before_whole: self.before_whole,
            back: self.back.clone(),
        }
    }

    /// Show `place` (another BAM's, with its contigs already turned into
    /// this BAM's numbers): its contig, left edge, zoom, cursor and the
    /// way back from both. A new contig resets this view as `place` does;
    /// its rows, selection, stretch, filters and panels are its own.
    pub fn follow(&mut self, place: StackPlace, length: u64) {
        if place.contig != self.contig {
            self.place(place.contig, place.cursor, length);
        }
        self.left = place.left;
        self.per_col = place.per_col;
        self.cursor = place.cursor;
        self.before_whole = place.before_whole;
        self.back = place.back;
    }

    /// As [`go`](Self::go), without remembering.
    pub fn place(&mut self, contig: usize, pos: u64, length: u64) {
        let fresh = contig != self.contig;
        if fresh {
            self.contig = contig;
            let levels = zoom_levels(length, self.width);
            if !levels.contains(&self.per_col) {
                self.per_col = *levels.last().expect("a level");
            }
            self.before_whole = None;
            self.clear_stretch();
            self.loaded = None;
            self.packed = Packed::default();
            self.row = 0;
            self.top = 0;
            self.generation += 1;
        }
        self.cursor = pos.clamp(1, length.max(1));
        if fresh || self.col_of(self.cursor).is_none() {
            let half = self.cols() / 2 * self.per_col;
            self.place_left(self.cursor.saturating_sub(half), length);
        }
    }

    /// The whole of `contig`, cursor at its start.
    pub fn whole_contig(&mut self, contig: usize, length: u64) {
        self.go(contig, 1, length);
        self.per_col = *zoom_levels(length, self.width).last().expect("a level");
        self.left = 1;
        self.before_whole = None;
        self.repack();
    }

    /// Show `a..=b` at the closest zoom that fits it, cursor at `a`.
    pub fn fit(&mut self, a: u64, b: u64, length: u64) {
        let levels = zoom_levels(length, self.width);
        let want = b.saturating_sub(a) + 1;
        self.per_col = levels
            .iter()
            .copied()
            .find(|&l| l * self.cols() >= want)
            .unwrap_or(*levels.last().expect("a level"));
        self.cursor = a.clamp(1, length.max(1));
        self.place_left(a, length);
        self.before_whole = None;
        self.repack();
    }

    /// Move the cursor's row by `by`, scrolling to keep it on screen. The
    /// screen may scroll one row past the last, to show what was not
    /// drawn.
    pub fn move_row(&mut self, by: isize) {
        let rows = self.packed.rows.len();
        self.row = self
            .row
            .saturating_add_signed(by)
            .min(rows.saturating_sub(1));
        let height = self.height.max(1);
        if self.row < self.top {
            self.top = self.row;
        } else if self.row >= self.top + height {
            self.top = self.row + 1 - height;
        }
        let extra = usize::from(self.packed.not_drawn > 0);
        if by > 0 && self.row + 1 == rows {
            self.top = self.top.max((rows + extra).saturating_sub(height));
        }
    }

    /// Scroll sideways by `cols` columns, leaving the cursor where it is.
    pub fn scroll(&mut self, cols: i64, length: u64) {
        let by = cols.saturating_mul(self.per_col as i64);
        self.place_left(self.left.saturating_add_signed(by), length);
    }

    /// Scroll the rows of reads by `by`, down to one row past the last
    /// when some were not drawn.
    pub fn scroll_rows(&mut self, by: isize) {
        let extra = usize::from(self.packed.not_drawn > 0);
        let most = (self.packed.rows.len() + extra).saturating_sub(self.height.max(1));
        self.top = self.top.saturating_add_signed(by).min(most);
    }

    /// The stretch of reference marked, or from the first `v` to the
    /// cursor while marking; `start <= end`.
    pub fn stretch(&self) -> Option<(u64, u64)> {
        self.marked.or_else(|| {
            self.marking
                .map(|at| (at.min(self.cursor), at.max(self.cursor)))
        })
    }

    /// `v`: start a stretch at the cursor, or finish it there. A stretch
    /// replaces the selected reads.
    pub fn mark(&mut self) {
        match self.marking.take() {
            Some(at) => self.marked = Some((at.min(self.cursor), at.max(self.cursor))),
            None => {
                self.marking = Some(self.cursor);
                self.marked = None;
                self.selected.clear();
            }
        }
    }

    /// Mark `a..=b` (either order), kept within the contig.
    pub fn set_stretch(&mut self, a: u64, b: u64, length: u64) {
        let length = length.max(1);
        let (a, b) = (a.min(b).clamp(1, length), a.max(b).clamp(1, length));
        self.marked = Some((a, b));
        self.marking = None;
        self.selected.clear();
    }

    pub fn clear_stretch(&mut self) {
        self.marked = None;
        self.marking = None;
    }

    /// Add `read` to the selection, once.
    pub fn select(&mut self, read: ReadId) {
        self.clear_stretch();
        if !self.selected.contains(&read) {
            self.selected.push(read);
        }
    }

    /// Every read drawn in rows `rows` that overlaps `a..=b`, row by row.
    pub fn touched(
        &self,
        (a, b): (u64, u64),
        rows: std::ops::RangeInclusive<usize>,
    ) -> Vec<ReadId> {
        let reads = self.reads();
        let mut out = Vec::new();
        for row in rows {
            let Some(row) = self.packed.rows.get(row) else {
                break;
            };
            for &i in row {
                let (s, e) = self.span(&reads[i]);
                if s <= b && e >= a {
                    out.push(ReadId::of(&reads[i]));
                }
            }
        }
        out
    }

    /// Put the cursor's row on the read [`focus`](Self::focus) names, once
    /// a window serving the view (`length` the contig's) has been read: a
    /// window read before the view moved would be packed again.
    pub fn apply_focus(&mut self, length: u64) {
        let Some((name, start)) = &self.focus else {
            return;
        };
        let Some(loaded) = &self.loaded else {
            return;
        };
        let Some(want) = self.wanted(length, pass_bin(length)) else {
            return;
        };
        let have = (loaded.window.region, loaded.window.bin, loaded.asked_reads);
        if !serves(&have, &self.view_region(length), &want) {
            return;
        }
        let region = loaded.window.region;
        if region.contig != self.contig || *start < region.start || *start > region.end {
            return;
        }
        let reads = self.reads();
        let found = self.packed.rows.iter().position(|row| {
            row.iter()
                .any(|&i| reads[i].name == *name && reads[i].start == *start)
        });
        self.focus = None;
        if let Some(row) = found {
            self.row = row;
            let height = self.height.max(1);
            if row < self.top || row >= self.top + height {
                self.top = row.saturating_sub(height / 2);
            }
        }
    }

    /// The read under the cursor: on the cursor's row, at its position.
    pub fn cursor_read(&self) -> Option<&Read> {
        let i = self.read_in_row(self.row, (self.cursor, self.cursor))?;
        self.reads().get(i)
    }

    /// The region shown.
    pub fn view_region(&self, length: u64) -> Region {
        Region {
            contig: self.contig,
            start: self.left,
            end: self.end(length),
        }
    }

    /// What to read for this view: the region (the view and a view's
    /// width each side), the bin size, and whether to read reads. `None`
    /// when the coverage pass (bins of `pass_bin` bases) or the sketch can
    /// draw it alone.
    pub fn wanted(&self, length: u64, pass_bin: u64) -> Option<Want> {
        let shown = self.cols() * self.per_col;
        let reads = shown < READS_WIDTH;
        if !reads && self.per_col >= pass_bin {
            return None;
        }
        let region = Region {
            contig: self.contig,
            start: self.left.saturating_sub(shown).max(1),
            end: (self.left + 2 * shown - 1).min(length),
        };
        Some((region, self.per_col, reads))
    }

    /// The stretch `read` takes on screen: with its clipped ends unless
    /// they are hidden.
    pub fn span(&self, read: &Read) -> (u64, u64) {
        if self.hide_clips {
            return (read.start, read.end);
        }
        let (lead, trail) = read.clips();
        (read.start.saturating_sub(lead).max(1), read.end + trail)
    }

    /// Stack the shown loaded reads afresh.
    pub fn repack(&mut self) {
        let reads = self.reads();
        let order: Vec<usize> = (0..reads.len())
            .filter(|&i| self.show_filtered || reads[i].counts())
            .collect();
        let spans: Vec<(u64, u64)> = order.iter().map(|&i| self.span(&reads[i])).collect();
        let mut packed = pack(&spans, self.per_col, MAX_ROWS);
        for row in &mut packed.rows {
            for i in row.iter_mut() {
                *i = order[*i];
            }
        }
        self.packed = packed;
        self.row = self.row.min(self.packed.rows.len().saturating_sub(1));
        self.top = self.top.min(self.packed.rows.len().saturating_sub(1));
    }

    /// The loaded reads, if they were read.
    pub fn reads(&self) -> &[Read] {
        self.loaded
            .as_ref()
            .and_then(|l| l.window.reads.as_deref())
            .unwrap_or_default()
    }

    /// The read drawn at column `col` of screen row `row` of reads.
    pub fn read_at(&self, col: usize, row: usize) -> Option<usize> {
        self.read_in_row(self.top + row, self.span_of(col))
    }

    /// The read in packed row `row` overlapping `a..=b`.
    pub fn read_in_row(&self, row: usize, (a, b): (u64, u64)) -> Option<usize> {
        let reads = self.reads();
        self.packed.rows.get(row)?.iter().copied().find(|&i| {
            let (s, e) = self.span(&reads[i]);
            s <= b && e >= a
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seqpeeq_core::bam::testing::fixture;
    use seqpeeq_core::bam::{Contig, Part, Place, Read, parse_region};
    use seqpeeq_core::{Command, Progress, Session};

    /// The fixture's mapped reads in `text`, and its contigs.
    pub(crate) fn fixture_reads_in(text: &str) -> (Vec<Read>, Vec<Contig>) {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let mut session = Session::new(None);
        session.set_cache_dir(dir.path().join("cache"));
        session
            .execute(Command::Open {
                path: f.bam,
                kind: None,
            })
            .unwrap();
        let source = session.datasets()[0].bam().unwrap().source().clone();
        let contigs = source.contigs().to_vec();
        let region = parse_region(text, &contigs, "reads.bam").unwrap().0;
        (source.reads(&region, &Progress::new()).unwrap(), contigs)
    }

    fn fixture_read(name: &str, flags: u16) -> (Read, Vec<Contig>) {
        let (mut reads, contigs) = fixture_reads_in("c1");
        let at = reads
            .iter()
            .position(|r| r.name == name && r.flags == flags)
            .unwrap();
        (reads.swap_remove(at), contigs)
    }

    #[test]
    fn zoom_levels_run_1_2_5_up_to_the_whole_contig() {
        assert_eq!(zoom_levels(1000, 80), [1, 2, 5, 10, 20]);
        assert_eq!(zoom_levels(50, 80), [1]);
        assert_eq!(*zoom_levels(250_000_000, 100).last().unwrap(), 5_000_000);
        assert_eq!(zoom_levels(10, 0), [1, 2, 5, 10], "no width is one column");
    }

    #[test]
    fn columns_map_to_positions_and_back() {
        let mut v = BamView::new(0, 1000, 80);
        v.per_col = 10;
        v.left = 101;
        assert_eq!(v.span_of(0), (101, 110));
        assert_eq!(v.col_of(125), Some(2));
        assert_eq!(v.col_of(100), None);
        assert_eq!(v.col_of(101 + 80 * 10), None, "past the last column");
        assert_eq!(v.end(1000), 900);
        assert_eq!(v.end(500), 500);
    }

    #[test]
    fn a_new_view_shows_the_whole_contig() {
        let v = BamView::new(3, 1000, 80);
        assert_eq!((v.contig, v.left, v.cursor, v.per_col), (3, 1, 1, 20));
    }

    #[test]
    fn zooming_keeps_the_cursor_where_it_is_on_screen() {
        let mut v = BamView::new(0, 1_000_000, 80);
        v.per_col = 1;
        v.left = 461;
        v.cursor = 500;
        v.zoom(1, 1_000_000);
        assert_eq!((v.per_col, v.col_of(500)), (2, Some(39)));
        assert_eq!((v.left - 1) % 2, 0, "aligned to whole columns");
        v.zoom(-1, 1_000_000);
        assert_eq!((v.per_col, v.col_of(500)), (1, Some(39)));
        v.zoom(-1, 1_000_000);
        assert_eq!(v.per_col, 1, "no closer than a base a column");
        v.toggle_whole(1_000_000);
        assert_eq!(
            (v.left, v.per_col),
            (1, *zoom_levels(1_000_000, 80).last().unwrap())
        );
        v.toggle_whole(1_000_000);
        assert_eq!((v.left, v.per_col), (461, 1));
    }

    #[test]
    fn the_cursor_moves_by_columns_and_the_view_follows() {
        let mut v = BamView::new(0, 1000, 10);
        v.per_col = 1;
        v.step(15, 1000);
        assert_eq!((v.cursor, v.left), (16, 7));
        v.step(-100, 1000);
        assert_eq!((v.cursor, v.left), (1, 1));
        v.step(5000, 1000);
        assert_eq!((v.cursor, v.end(1000)), (1000, 1000));
    }

    #[test]
    fn going_somewhere_remembers_where_from_and_centres_it() {
        let mut v = BamView::new(0, 1000, 10);
        v.per_col = 1;
        v.go(0, 500, 1000);
        assert_eq!((v.cursor, v.left, v.back.clone()), (500, 495, vec![(0, 1)]));
        v.go(0, 498, 1000);
        assert_eq!(v.left, 495, "already on screen: the view stays");
        v.go(1, 9999, 500);
        assert_eq!((v.contig, v.cursor), (1, 500), "cut to the contig");
    }

    #[test]
    fn packing_is_first_fit_with_a_gap() {
        let spans = [(1, 10), (5, 20), (12, 30), (22, 40)];
        let p = pack(&spans, 1, 1000);
        assert_eq!(p.rows, [vec![0, 2], vec![1, 3]]);
        assert_eq!(p.not_drawn, 0);
        assert_eq!(pack(&spans, 1, 1).not_drawn, 2);
        assert_eq!(pack(&spans, 2, 1000).rows, [vec![0, 3], vec![1], vec![2]]);
    }

    fn long_cigar_read() -> Read {
        let (mut r, _) = fixture_read("r1", 99);
        r.cigar = [('M', 10), ('I', 1), ('M', 10), ('D', 1)]
            .iter()
            .copied()
            .cycle()
            .take(20)
            .collect();
        r.mate = None;
        r
    }

    #[test]
    fn the_details_line_gives_an_insertion_at_the_cursor() {
        let (mut r, contigs) = fixture_read("r1", 99);
        r.cigar = vec![('M', 10), ('I', 2), ('M', 38)];
        r.end = 148;
        r.mate = None;
        let bases = String::from_utf8(r.seq[10..12].to_vec()).unwrap();
        assert_eq!(
            details(&r, &contigs, 111),
            format!(
                "r1 · c1:101-148 (+) · ins 2 bp {bases} before 111 · MAPQ 60 · 50 bp · 10M2I38M"
            )
        );
        assert!(!details(&r, &contigs, 112).contains("ins"));
        r.cigar = vec![('M', 10), ('I', 30), ('M', 10)];
        let long = details(&r, &contigs, 111);
        assert!(long.contains("ins 30 bp "), "{long}");
        assert!(long.contains("… before 111"), "{long}");
    }

    #[test]
    fn the_details_line_says_where_everything_is() {
        let (r1, contigs) = fixture_read("r1", 99);
        assert_eq!(
            details(&r1, &contigs, 0),
            "r1 · c1:101-150 (+) · MAPQ 60 · 50 bp · 50M · mate c1:301 (−)"
        );
        let (r2, _) = fixture_read("r2", 0);
        assert_eq!(
            details(&r2, &contigs, 0),
            "r2 · c1:121-165 (+) · MAPQ 60 · 50 bp · 10S30M5D10M · part c2:51 (+) 10M40H"
        );
        assert!(
            details(&long_cigar_read(), &contigs, 0)
                .ends_with(" · 105 bp · 10M1I10M1D … 10M1I10M1D"),
            "{}",
            details(&long_cigar_read(), &contigs, 0)
        );
    }

    #[test]
    fn read_kinds_and_the_filter() {
        assert_eq!(read_kind(&fixture_read("r2", 0).0), ReadKind::Split);
        assert_eq!(
            read_kind(&fixture_read("r6", 97).0),
            ReadKind::MateElsewhere
        );
        let (mut improper, _) = fixture_read("r1", 99);
        improper.flags = 97;
        assert_eq!(read_kind(&improper), ReadKind::Improper);
        assert_eq!(read_kind(&fixture_read("r1", 99).0), ReadKind::Plain);
        assert_eq!(read_kind(&fixture_read("r3", 0).0), ReadKind::Plain);
        let _ = Part {
            place: Place {
                contig: 0,
                pos: 1,
                reverse: false,
            },
            cigar: String::new(),
            mapq: 0,
            nm: None,
        };

        let mut v = BamView::new(0, 1000, 80);
        v.per_col = 1;
        let (reads, _) = fixture_reads_in("c1:701-760");
        load(&mut v, reads);
        v.repack();
        assert_eq!(v.packed.rows.concat(), [0], "r4 and r5 do not count");
        v.show_filtered = true;
        v.repack();
        assert_eq!(v.packed.rows.concat(), [0, 1, 2]);
    }

    #[test]
    fn the_window_wanted_follows_the_zoom() {
        let mut v = BamView::new(0, 250_000_000, 100);
        assert_eq!(
            v.wanted(250_000_000, 61_036),
            None,
            "the pass or sketch draws it"
        );
        v.per_col = 2_000;
        v.left = 1_000_001;
        let (r, bin, reads) = v.wanted(250_000_000, 61_036).unwrap();
        assert_eq!(
            (r.start, r.end, bin, reads),
            (800_001, 1_400_000, 2_000, false)
        );
        v.per_col = 1;
        let (r, bin, reads) = v.wanted(250_000_000, 61_036).unwrap();
        assert_eq!((r.start, r.end, bin, reads), (999_901, 1_000_200, 1, true));
        // A short contig whose pass has a bin per base still reads its reads.
        let v = BamView::new(0, 1000, 80);
        let (r, bin, reads) = v.wanted(1000, 1).unwrap();
        assert_eq!((r.start, r.end, bin, reads), (1, 1000, 20, true));
    }

    /// Hand `v` these reads as if a window had been read.
    pub(crate) fn load(v: &mut BamView, reads: Vec<Read>) {
        let count = reads.len();
        v.loaded = Some(Loaded {
            window: seqpeeq_core::bam::Window {
                region: seqpeeq_core::bam::Region {
                    contig: v.contig,
                    start: 1,
                    end: 1,
                },
                bin: 1,
                depth: Vec::new(),
                reads: Some(reads),
                count,
            },
            asked_reads: true,
            reference: None,
            with_reference: false,
        });
    }

    #[test]
    fn packed_reads_are_found_by_column_and_row() {
        let mut v = BamView::new(0, 1000, 80);
        v.per_col = 1;
        v.left = 101;
        let (reads, _) = fixture_reads_in("c1:101-160");
        load(&mut v, reads);
        v.repack();
        // r1 101-150 and r2 111-165 (with its clip) overlap: two rows.
        assert_eq!(v.packed.rows, [vec![0], vec![1]]);
        assert_eq!(v.read_at(0, 0), Some(0));
        assert_eq!(v.read_at(0, 1), None, "r2's clip starts at 111");
        assert_eq!(v.read_at(10, 1), Some(1));
        v.hide_clips = true;
        v.repack();
        assert_eq!(v.read_at(10, 1), None, "clips hidden");
        assert_eq!(v.read_at(20, 1), Some(1));
    }

    #[test]
    fn zoomed_out_reads_take_their_clips_too() {
        let mut v = BamView::new(0, 1000, 80);
        v.per_col = 5;
        v.left = 101;
        let (reads, _) = fixture_reads_in("c1:101-160");
        load(&mut v, reads);
        v.repack();
        assert_eq!(v.span(&v.reads()[1]), (111, 165), "r2 with its 10S");
        v.hide_clips = true;
        assert_eq!(v.span(&v.reads()[1]), (121, 165));
    }

    fn contig(name: &str, length: u64, mapped: u64) -> Contig {
        Contig {
            name: name.to_string(),
            length,
            mapped: Some(mapped),
            unmapped: Some(0),
        }
    }

    #[test]
    fn the_contig_list_filters_sorts_and_hides_empty_contigs() {
        let contigs = [
            contig("c1", 1000, 7),
            contig("c2", 500, 3),
            contig("c3", 300, 0),
            contig("x12", 50, 9),
        ];
        let mut list = ContigList::new(0, &contigs);
        assert_eq!(list.rows(&contigs), [0, 1, 3], "c3 has no reads");
        assert_eq!(list.hidden, 1);
        list.all = true;
        assert_eq!(list.rows(&contigs), [0, 1, 2, 3]);
        list.filter.set("2");
        assert_eq!(list.rows(&contigs), [1, 3]);
        list.sort = ContigSort::Reads;
        assert_eq!(list.rows(&contigs), [3, 1]);
        list.filter.set("X");
        assert_eq!(list.rows(&contigs), [3], "any case");
    }

    #[test]
    fn names_sort_with_their_numbers_in_order() {
        let contigs = [
            contig("chr10", 1, 1),
            contig("chr2", 1, 1),
            contig("chr1", 1, 1),
            contig("chrX", 1, 1),
        ];
        assert_eq!(ContigList::new(0, &contigs).rows(&contigs), [2, 1, 0, 3]);
    }

    #[test]
    fn many_contigs_filter_quickly() {
        let contigs: Vec<Contig> = (0..100_000)
            .map(|i| contig(&format!("contig_{i}"), 1000, i % 3))
            .collect();
        let start = std::time::Instant::now();
        let mut list = ContigList::new(0, &contigs);
        assert!(start.elapsed().as_secs_f64() < 1.0, "{:?}", start.elapsed());
        list.filter.set("7");
        let start = std::time::Instant::now();
        let rows = list.rows(&contigs);
        assert!(
            start.elapsed() < crate::time_budget(50),
            "{:?}",
            start.elapsed()
        );
        assert!(rows.len() > 1000);
    }
}
