//! Drawing the BAM tab: the whole-contig strip, the ruler, the reference,
//! coverage, the reads, and the details of the read under the cursor.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use seqpeeq_core::bam::{Contig, Coverage, Piece, Read, grouped};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::bam_view::{BamView, ContigList, ContigSort, READS_WIDTH, ReadId, details, read_kind};
use crate::palette;

/// Where each row of the BAM tab goes. A row not shown has no height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BamLayout {
    pub strip: Rect,
    pub ruler: Rect,
    /// Only at a base a column.
    pub reference: Rect,
    pub coverage: Rect,
    pub reads: Rect,
    pub details: Rect,
}

/// Lay the tab out in `area`. When it is short, rows go in this order
/// until the reads keep one: the details, the reference, the strip, the
/// ruler, the coverage.
pub fn bam_layout(area: Rect, view: &BamView) -> BamLayout {
    // strip, ruler, reference, coverage, details
    let mut keep = [true, true, view.per_col == 1, true, true];
    let kept = |keep: &[bool; 5]| keep.iter().filter(|&&k| k).count() as u16;
    for i in [4, 2, 0, 1, 3] {
        if kept(&keep) < area.height {
            break;
        }
        keep[i] = false;
    }
    let mut y = area.y;
    let mut take = |height: u16| {
        let height = height.min(area.bottom().saturating_sub(y));
        let rect = Rect { y, height, ..area };
        y += height;
        rect
    };
    let strip = take(u16::from(keep[0]));
    let ruler = take(u16::from(keep[1]));
    let reference = take(u16::from(keep[2]));
    let coverage = take(u16::from(keep[3]));
    let reads = take(area.height.saturating_sub(kept(&keep)));
    let details = take(u16::from(keep[4]));
    BamLayout {
        strip,
        ruler,
        reference,
        coverage,
        reads,
        details,
    }
}

/// One BAM's rows in a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberRows {
    /// Its file name, marked when it has the focus.
    pub name: Rect,
    pub coverage: Rect,
    pub reads: Rect,
}

/// Where each row of a stack of BAMs goes: the rows they share, and each
/// BAM's own. A row not shown has no height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackLayout {
    pub strip: Rect,
    pub ruler: Rect,
    pub reference: Rect,
    pub details: Rect,
    pub members: Vec<MemberRows>,
}

impl StackLayout {
    /// The layout as member `i` sees it: the shared rows, and its own
    /// coverage and reads.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the stack view uses it from the next commit")
    )]
    pub fn member(&self, i: usize) -> BamLayout {
        BamLayout {
            strip: self.strip,
            ruler: self.ruler,
            reference: self.reference,
            coverage: self.members[i].coverage,
            reads: self.members[i].reads,
            details: self.details,
        }
    }
}

/// Lay a stack out in `area`, `needs` being the rows of reads each BAM
/// has (`usize::MAX` while unknown). The shared strip, ruler and
/// reference (at a base a column) go on top and the details at the
/// bottom; each BAM has a name line, a coverage row and at least one row
/// of reads. When that does not fit, rows go in this order: the details,
/// the reference, the strip, the ruler, then every coverage row. The
/// other rows go to the BAMs whose reads need them.
pub fn stack_layout(area: Rect, base_level: bool, needs: &[usize]) -> StackLayout {
    let n = needs.len() as u16;
    // strip, ruler, reference, details, coverage
    let mut keep = [true, true, base_level, true, true];
    let least = |keep: &[bool; 5]| {
        let shared = keep[..4].iter().filter(|&&k| k).count() as u16;
        shared + n * (2 + u16::from(keep[4]))
    };
    for i in [3, 2, 0, 1, 4] {
        if least(&keep) <= area.height {
            break;
        }
        keep[i] = false;
    }
    let spare = area.height.saturating_sub(least(&keep));
    let wants: Vec<usize> = needs.iter().map(|need| need.saturating_sub(1)).collect();
    let extra = share(usize::from(spare), &wants);

    let mut y = area.y;
    let mut take = |height: u16| {
        let height = height.min(area.bottom().saturating_sub(y));
        let rect = Rect { y, height, ..area };
        y += height;
        rect
    };
    let strip = take(u16::from(keep[0]));
    let ruler = take(u16::from(keep[1]));
    let reference = take(u16::from(keep[2]));
    let members = extra
        .iter()
        .map(|&more| MemberRows {
            name: take(1),
            coverage: take(u16::from(keep[4])),
            reads: take(1 + more as u16),
        })
        .collect();
    let details = take(u16::from(keep[3]));
    StackLayout {
        strip,
        ruler,
        reference,
        details,
        members,
    }
}

/// Share `spare` rows out among wants: each want no bigger than an even
/// share is met in full, and what is left is split evenly among the rest
/// (or, when every want is met, among all), the first getting any odd
/// rows.
fn share(mut spare: usize, wants: &[usize]) -> Vec<usize> {
    let mut got = vec![0; wants.len()];
    let mut open: Vec<usize> = (0..wants.len()).collect();
    while let Some(at) = open.iter().position(|&i| wants[i] <= spare / open.len()) {
        let i = open.remove(at);
        got[i] = wants[i];
        spare -= wants[i];
    }
    let rest: Vec<usize> = if open.is_empty() {
        (0..wants.len()).collect()
    } else {
        open
    };
    if let Some(each) = spare.checked_div(rest.len()) {
        let odd = spare % rest.len();
        for (k, &i) in rest.iter().enumerate() {
            got[i] += each + usize::from(k < odd);
        }
    }
    got
}

const BARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A bar for `value` out of `most`: blank for nothing, at least the
/// lowest bar for anything.
fn bar(value: f32, most: f32) -> char {
    if value <= 0.0 || most <= 0.0 {
        return BARS[0];
    }
    let step = 1 + ((value / most) * 7.0).round() as usize;
    BARS[step.min(8)]
}

/// The mean of the bins of `values` (spread evenly over `1..=length`)
/// that overlap `a..=b`.
fn resample(values: &[f32], length: u64, a: u64, b: u64) -> f32 {
    let n = values.len() as u64;
    if n == 0 || length == 0 || b < a {
        return 0.0;
    }
    let first = ((a - 1) * n / length).min(n - 1) as usize;
    let last = ((b.min(length) - 1) * n / length).min(n - 1) as usize;
    let part = &values[first..=last.max(first)];
    part.iter().sum::<f32>() / part.len() as f32
}

/// The strip's head: the contig's name, length and reads.
fn strip_head(contig: &Contig) -> String {
    let reads = contig.mapped.map_or("?".to_string(), grouped);
    format!(
        "{}  {} bp  {reads} reads  ",
        contig.name,
        grouped(contig.length)
    )
}

/// The part of the strip that pictures the whole contig.
pub fn strip_bar(strip: Rect, contig: &Contig) -> Rect {
    let head = (strip_head(contig).width() as u16).min(strip.width);
    Rect {
        x: strip.x + head,
        width: strip.width - head,
        ..strip
    }
}

/// What is under a point of the BAM tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BamHit {
    /// The whole-contig strip, at this position.
    Strip(u64),
    /// The ruler, reference or coverage, at the first position of a column.
    Ruler(u64),
    /// The reads, at a column and a row counted from the first shown.
    Read {
        col: usize,
        row: usize,
    },
    Nothing,
}

pub fn bam_hit(
    layout: &BamLayout,
    view: &BamView,
    contig: &Contig,
    column: u16,
    row: u16,
) -> BamHit {
    let at = Position::new(column, row);
    let bar = strip_bar(layout.strip, contig);
    if bar.contains(at) {
        let (pos, _) = strip_span(column - bar.x, bar.width, contig.length);
        return BamHit::Strip(pos);
    }
    let col = (column.saturating_sub(layout.reads.x)) as usize;
    let past_end = view.span_of(col).0 > contig.length;
    for band in [layout.ruler, layout.reference, layout.coverage] {
        if band.contains(at) && !past_end {
            return BamHit::Ruler(view.span_of(col).0);
        }
    }
    if layout.reads.contains(at) {
        return BamHit::Read {
            col,
            row: (row - layout.reads.y) as usize,
        };
    }
    BamHit::Nothing
}

/// The positions column `i` of a `width`-column strip covers.
pub fn strip_span(i: u16, width: u16, length: u64) -> (u64, u64) {
    let (i, width) = (u64::from(i), u64::from(width.max(1)));
    let a = 1 + i * length / width;
    (a, ((i + 1) * length / width).max(a))
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(ch).set_style(style);
    }
}

/// The reference base at each position the view can show, from the
/// reference read with the window or else from the reads' MD tags.
fn reference_bases(view: &BamView, length: u64) -> HashMap<u64, u8> {
    let mut bases = HashMap::new();
    let Some(loaded) = &view.loaded else {
        return bases;
    };
    let (left, end) = (view.left, view.end(length));
    if let Some(reference) = &loaded.reference {
        let start = loaded.window.region.start;
        for pos in left.max(start)..=end {
            if let Some(&b) = reference.get((pos - start) as usize) {
                bases.insert(pos, b.to_ascii_uppercase());
            }
        }
        return bases;
    }
    for read in view.reads() {
        if read.end < left || read.start > end {
            continue;
        }
        for (pos, b) in read.md_reference().unwrap_or_default() {
            if (left..=end).contains(&pos) {
                bases.insert(pos, b);
            }
        }
    }
    bases
}

/// The rows of contigs the contig list shows: the body less its border,
/// the filter and the column heads.
pub fn contig_list_rows(app: &App) -> usize {
    let screen = Rect::new(0, 0, app.screen.width, app.screen.height);
    crate::ui::regions(app, screen)
        .body
        .height
        .saturating_sub(4) as usize
}

/// Where a click lands in the contig list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContigHit {
    /// On a contig: its index into `ContigList::rows`.
    Row(usize),
    Heading,
    /// Anywhere else in the box, its border too.
    Inside,
    Outside,
}

/// What is at `column`, `row` in the contig list, as `draw_contigs` lays
/// it out over the body.
pub fn contig_hit(app: &App, list: &ContigList, column: u16, row: u16) -> ContigHit {
    use ratatui::widgets::Block;
    let screen = Rect::new(0, 0, app.screen.width, app.screen.height);
    let area = crate::ui::regions(app, screen).body;
    let at = Position::new(column, row);
    if !area.contains(at) {
        return ContigHit::Outside;
    }
    let inner = Block::bordered().inner(area);
    if !inner.contains(at) {
        return ContigHit::Inside;
    }
    if row == inner.y + 1 {
        return ContigHit::Heading;
    }
    let Some(line) = (row - inner.y).checked_sub(2) else {
        return ContigHit::Inside;
    };
    let shown = app
        .bam()
        .filter(|(d, _, _)| d.id == list.dataset)
        .map_or(0, |(_, doc, _)| list.rows(doc.source().contigs()).len());
    let i = list.top + line as usize;
    if i < shown {
        ContigHit::Row(i)
    } else {
        ContigHit::Inside
    }
}

/// `c`: the contig list over the body.
pub fn draw_contigs(frame: &mut Frame, app: &App, list: &ContigList, area: Rect) {
    use ratatui::widgets::{Block, Clear};
    let Some((_, doc, _)) = app.bam().filter(|(d, _, _)| d.id == list.dataset) else {
        return;
    };
    let contigs = doc.source().contigs();
    let rows = list.rows(contigs);
    let mut title = format!("{} contigs", grouped(rows.len() as u64));
    if !list.all && list.hidden > 0 {
        title.push_str(&format!(
            " ({} with no reads hidden; Ctrl-A shows them)",
            grouped(list.hidden as u64)
        ));
    }
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let width = inner.width as usize;
    let buf = frame.buffer_mut();
    let lead = "filter: ";
    buf.set_stringn(inner.x, inner.y, lead, width, Style::new());
    buf.set_stringn(
        inner.x + lead.len() as u16,
        inner.y,
        list.filter.text(),
        width.saturating_sub(lead.len()),
        Style::new(),
    );
    let sorted = match list.sort {
        ContigSort::Name => "by name (Tab: by reads)",
        ContigSort::Reads => "by reads (Tab: by name)",
    };
    let name_width = width.saturating_sub(24).max(1);
    let line = |name: &str, length: &str, reads: &str| {
        let name: String = name.chars().take(name_width).collect();
        format!("{name:<name_width$} {length:>12} {reads:>10}")
    };
    if inner.height > 1 {
        let heads = line(&format!("name, {sorted}"), "length", "reads");
        let dim = Style::new().add_modifier(Modifier::DIM);
        buf.set_stringn(inner.x, inner.y + 1, heads, width, dim);
    }
    for (line_no, &i) in rows.iter().enumerate().skip(list.top) {
        let y = inner.y + 2 + (line_no - list.top) as u16;
        if y >= inner.bottom() {
            break;
        }
        let c = &contigs[i];
        let reads = c.mapped.map_or("?".to_string(), grouped);
        let style = if line_no == list.cursor {
            palette::SELECTED
        } else {
            Style::new()
        };
        buf.set_stringn(
            inner.x,
            y,
            line(&c.name, &grouped(c.length), &reads),
            width,
            style,
        );
    }
    let x = inner.x + (lead.len() + list.filter.text()[..list.filter.cursor()].width()) as u16;
    if x < inner.right() {
        frame.set_cursor_position(Position::new(x, inner.y));
    }
}

/// Whether the view can say where reads differ from the reference.
pub fn knows_reference(app: &App) -> bool {
    let Some((_, doc, view)) = app.bam() else {
        return true;
    };
    doc.source().reference().is_some() || view.reads().iter().any(|r| r.md.is_some())
}

pub fn draw_bam(frame: &mut Frame, app: &App, area: Rect) {
    let Some((dataset, doc, view)) = app.bam() else {
        return;
    };
    let source = doc.source();
    let contig = &source.contigs()[view.contig];
    let length = contig.length;
    let layout = bam_layout(area, view);
    let pass = app.passes.get(&(dataset.id, view.contig));
    let finished = pass.filter(|p| p.done_to >= length);
    let buf = frame.buffer_mut();

    // The strip: the sketch from the index until the pass is done.
    if layout.strip.height > 0 {
        let head = strip_head(contig);
        buf.set_stringn(
            layout.strip.x,
            layout.strip.y,
            &head,
            layout.strip.width as usize,
            Style::new(),
        );
        let bar_area = strip_bar(layout.strip, contig);
        let width = bar_area.width;
        let values: Vec<f32> = match finished {
            Some(pass) => (0..width)
                .map(|i| {
                    let (a, b) = strip_span(i, width, length);
                    resample(&pass.bins, length, a, b)
                })
                .collect(),
            None => source.sketch(view.contig, width as usize),
        };
        let most = values.iter().copied().fold(0.0, f32::max);
        let (left, end) = (view.left, view.end(length));
        for (i, &value) in values.iter().enumerate() {
            let (a, b) = strip_span(i as u16, width, length);
            let style = if a <= end && b >= left {
                palette::IN_VIEW
            } else {
                palette::DEPTH
            };
            let style = palette::DEPTH.patch(style);
            put(
                buf,
                bar_area.x + i as u16,
                bar_area.y,
                bar(value, most),
                style,
            );
        }
    }

    draw_ruler(buf, layout.ruler, view, length);

    let bases = if view.per_col == 1 {
        reference_bases(view, length)
    } else {
        HashMap::new()
    };
    if layout.reference.height > 0 {
        for col in 0..layout.reference.width {
            let pos = view.left + u64::from(col);
            if pos > length {
                break;
            }
            if let Some(&b) = bases.get(&pos) {
                put(
                    buf,
                    layout.reference.x + col,
                    layout.reference.y,
                    b as char,
                    palette::base(b),
                );
            }
        }
    }

    highlight_stretch(buf, layout.reference, view);

    if layout.coverage.height > 0 {
        let depths = column_depths(view, length, pass, source);
        let most = depths.iter().copied().fold(0.0, f32::max);
        for (col, &depth) in depths.iter().enumerate() {
            put(
                buf,
                layout.coverage.x + col as u16,
                layout.coverage.y,
                bar(depth, most),
                palette::DEPTH,
            );
        }
    }

    draw_reads(buf, layout.reads, view, length, &bases);

    if layout.details.height > 0
        && let Some(read) = view.cursor_read()
    {
        buf.set_stringn(
            layout.details.x,
            layout.details.y,
            details(read, source.contigs(), view.cursor),
            layout.details.width as usize,
            Style::new(),
        );
    }

    if matches!(app.mode, Mode::Normal)
        && let Some(col) = view.col_of(view.cursor)
        && (col as u16) < area.width
    {
        let on_screen = view.row >= view.top && view.row - view.top < layout.reads.height as usize;
        let y = if on_screen {
            layout.reads.y + (view.row - view.top) as u16
        } else {
            layout.ruler.y
        };
        if y < area.bottom() {
            frame.set_cursor_position(Position::new(area.x + col as u16, y));
        }
    }
}

/// The tick spacing for the ruler: 1, 2 or 5 times a power of ten, at
/// least `n`.
pub(crate) fn nice(n: u64) -> u64 {
    let mut decade = 1u64;
    loop {
        for step in [1, 2, 5] {
            let tick = decade.saturating_mul(step);
            if tick >= n || tick == u64::MAX {
                return tick;
            }
        }
        decade = decade.saturating_mul(10);
    }
}

fn draw_ruler(buf: &mut Buffer, area: Rect, view: &BamView, length: u64) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let end = view.end(length);
    let spacing = nice(view.per_col * 10);
    let mut labels = vec![view.left];
    let mut tick = view.left.div_ceil(spacing) * spacing;
    while tick <= end {
        if tick > view.left {
            labels.push(tick);
        }
        tick += spacing;
    }
    let mut free = 0u16;
    for pos in labels {
        let Some(col) = view.col_of(pos) else {
            continue;
        };
        let col = col as u16;
        let text = pos.to_string();
        if col < free || col + text.len() as u16 > area.width {
            continue;
        }
        buf.set_string(area.x + col, area.y, &text, Style::new());
        free = col + text.len() as u16 + 1;
    }
    highlight_stretch(buf, area, view);
    if let Some(col) = view
        .col_of(view.cursor)
        .filter(|&c| (c as u16) < area.width)
        && let Some(cell) = buf.cell_mut(Position::new(area.x + col as u16, area.y))
    {
        cell.set_style(palette::SELECTED);
    }
}

/// Patch the stretch's style onto the columns of `area` it meets.
fn highlight_stretch(buf: &mut Buffer, area: Rect, view: &BamView) {
    let Some((a, b)) = view.stretch() else {
        return;
    };
    if area.height == 0 {
        return;
    }
    for col in 0..area.width {
        let (start, end) = view.span_of(col as usize);
        if start <= b
            && end >= a
            && let Some(cell) = buf.cell_mut(Position::new(area.x + col, area.y))
        {
            cell.set_style(palette::STRETCH);
        }
    }
}

/// The mean depth under each column in view, when the loaded window was
/// read at this zoom and covers the view.
fn window_depths(view: &BamView, length: u64) -> Option<Vec<f32>> {
    let end = view.end(length);
    let w = &view.loaded.as_ref()?.window;
    let serves = w.region.contig == view.contig
        && w.bin == view.per_col
        && w.region.start <= view.left
        && w.region.end >= end;
    serves.then(|| {
        (0..view.width)
            .map(|c| view.span_of(c).0)
            .take_while(|&a| a <= end)
            .map(|a| {
                let i = ((a - w.region.start) / w.bin) as usize;
                w.depth.get(i).copied().unwrap_or(0.0)
            })
            .collect()
    })
}

/// The depth under the cursor, `depth 7` a base a column or `mean depth
/// 7.3` zoomed out; nothing until the reads in view are read.
pub fn depth_text(view: &BamView, length: u64) -> Option<String> {
    let col = view.col_of(view.cursor)?;
    let depth = *window_depths(view, length)?.get(col)?;
    Some(if view.per_col == 1 {
        format!("depth {}", depth.round() as u64)
    } else {
        format!("mean depth {depth:.1}")
    })
}

/// The mean depth under each column in view: from the window when it was
/// read at this zoom, else from the coverage pass, else from the sketch.
fn column_depths(
    view: &BamView,
    length: u64,
    pass: Option<&Coverage>,
    source: &seqpeeq_core::bam::BamSource,
) -> Vec<f32> {
    let end = view.end(length);
    let cols: Vec<(u64, u64)> = (0..view.width)
        .map(|c| view.span_of(c))
        .take_while(|&(a, _)| a <= end)
        .map(|(a, b)| (a, b.min(length)))
        .collect();
    if let Some(depths) = window_depths(view, length) {
        return depths;
    }
    // A pass still running has zeros past its front; the sketch stands
    // in until it is done.
    let values = match pass.filter(|p| p.done_to >= length) {
        Some(pass) => pass.bins.clone(),
        None => source.sketch(view.contig, crate::bam_view::PASS_BINS as usize),
    };
    cols.iter()
        .map(|&(a, b)| resample(&values, length, a, b))
        .collect()
}

fn draw_reads(buf: &mut Buffer, area: Rect, view: &BamView, length: u64, bases: &HashMap<u64, u8>) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let dim = Style::new().add_modifier(Modifier::DIM);
    let say = |buf: &mut Buffer, text: &str| {
        buf.set_stringn(area.x, area.y, text, area.width as usize, dim);
    };
    if view.width as u64 * view.per_col >= READS_WIDTH {
        return say(buf, "zoom in to see reads");
    }
    let Some(loaded) = view
        .loaded
        .as_ref()
        .filter(|l| l.asked_reads && l.window.region.contig == view.contig)
    else {
        return;
    };
    let Some(reads) = &loaded.window.reads else {
        return say(buf, "zoom in to see reads");
    };
    let rows = view.packed.rows.len();
    for line in 0..area.height {
        let row = view.top + line as usize;
        let y = area.y + line;
        if row < rows {
            for &i in &view.packed.rows[row] {
                draw_read(buf, area, y, view, length, &reads[i], bases);
            }
        } else if row == rows && view.packed.not_drawn > 0 {
            let text = format!("+{} reads not drawn", grouped(view.packed.not_drawn as u64));
            buf.set_stringn(area.x, y, text, area.width as usize, dim);
        }
    }
}

fn draw_read(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    view: &BamView,
    length: u64,
    read: &Read,
    bases: &HashMap<u64, u8>,
) {
    let mut style = palette::read(read_kind(read));
    if read.mapq == 0 || !read.counts() {
        style = style.add_modifier(Modifier::DIM);
    }
    if view.selected.contains(&ReadId::of(read)) {
        style = style.patch(palette::SELECTED);
    }
    let end = view.end(length);
    let at = |pos: u64| {
        view.col_of(pos)
            .filter(|&c| (c as u16) < area.width && pos <= end)
            .map(|c| area.x + c as u16)
    };
    let reverse = read.reverse();
    if view.per_col > 1 {
        let (a, b) = (read.start.max(view.left), read.end.min(end));
        if a > b {
            return;
        }
        let (Some(first), Some(last)) = (at(a), at(b)) else {
            return;
        };
        for x in first..=last {
            put(buf, x, y, '━', style);
        }
        if !reverse && read.end <= end {
            put(buf, last, y, '▸', style);
        } else if reverse && read.start >= view.left {
            put(buf, first, y, '◂', style);
        }
        return;
    }
    let mut inserted = Vec::new();
    for piece in read.pieces() {
        match piece {
            Piece::Base { pos, base } => {
                let Some(x) = at(pos) else { continue };
                let differs = match (base, bases.get(&pos)) {
                    (Some(b), Some(r)) => !b.eq_ignore_ascii_case(r),
                    _ => false,
                };
                match base.filter(|_| differs) {
                    Some(b) => {
                        let ch = if reverse {
                            b.to_ascii_lowercase()
                        } else {
                            b.to_ascii_uppercase()
                        };
                        put(buf, x, y, ch as char, style.patch(palette::base(b)));
                    }
                    None => put(buf, x, y, if reverse { ',' } else { '.' }, style),
                }
            }
            Piece::Deleted { pos } => {
                if let Some(x) = at(pos) {
                    put(buf, x, y, '*', style);
                }
            }
            Piece::Skipped { start, end: last } => {
                for pos in start.max(view.left)..=last.min(end) {
                    if let Some(x) = at(pos) {
                        put(buf, x, y, '─', style);
                    }
                }
            }
            Piece::Inserted { before, .. } => inserted.push(before),
            Piece::Clipped { pos, base } if !view.hide_clips => {
                if let Some(x) = at(pos) {
                    let faint = style.add_modifier(Modifier::DIM);
                    put(buf, x, y, base.to_ascii_lowercase() as char, faint);
                }
            }
            Piece::Clipped { .. } => {}
        }
    }
    for before in inserted {
        if let Some(x) = at(before)
            && let Some(cell) = buf.cell_mut(Position::new(x, y))
        {
            cell.set_style(palette::INSERTION);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::app::bam::tests::{id, with_bam};
    use crate::palette;
    use crate::ui::tests::{render, rows, settle};
    use ratatui::layout::Size;
    use seqpeeq_core::bam::testing::{bam_from_sam, ref_bases};
    use seqpeeq_core::{BamCmd, Command, Session};
    use tempfile::TempDir;

    /// Tell `app` the screen is `width` by `height` and let it catch up.
    fn sized(app: &mut App, width: u16, height: u16) {
        app.resize(Size::new(width, height));
        settle(app);
        // A long "opened …" would wrap and push the rows up.
        app.status.clear();
    }

    /// Show base level from `pos` on the current contig, cursor there.
    fn base_level_at(app: &mut App, pos: u64) {
        let id = id(app);
        let view = app.bams.get_mut(&id).unwrap();
        view.per_col = 1;
        view.left = pos;
        view.cursor = pos;
        view.repack();
        settle(app);
    }

    /// The fixture at base level from `pos` on c1, on an 80 x 14 screen.
    fn bam_at(pos: u64, with_ref: bool) -> (App, TempDir) {
        let (mut app, dir) = with_bam();
        if with_ref {
            app.enqueue(Command::Bam(BamCmd::Ref {
                file: dir.path().join("ref.fa"),
            }));
        }
        sized(&mut app, 80, 14);
        base_level_at(&mut app, pos);
        (app, dir)
    }

    /// An app showing a BAM made from `sam`, on an 80 x 14 screen.
    fn with_sam(sam: &str) -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let bam = bam_from_sam(dir.path(), "t.bam", sam);
        let mut app = App::new(Session::new(None));
        app.enqueue(Command::Open {
            path: bam,
            kind: None,
        });
        settle(&mut app);
        sized(&mut app, 80, 14);
        (app, dir)
    }

    const HEAD: &str = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:1000\n";

    #[test]
    fn bam_base_level() {
        let (app, _dir) = bam_at(101, true);
        let terminal = render(&app, 80, 14);
        insta::assert_snapshot!(terminal.backend());
        let screen = rows(&terminal);
        assert!(
            screen[1].starts_with("c1  1,000 bp  7 reads  "),
            "{}",
            screen[1]
        );
        assert!(screen[2].starts_with("101"), "{}", screen[2]);
        assert_eq!(screen[3], ref_bases("c1", 101, 180));
        assert!(screen[5].starts_with(".........G........"), "{}", screen[5]);
        assert!(
            screen[6].contains("..........*****.........."),
            "{}",
            screen[6]
        );
        assert!(
            screen[11].starts_with("r1 · c1:101-150 (+)"),
            "{}",
            screen[11]
        );
        assert!(screen[12].starts_with("c1:101"), "{}", screen[12]);
    }

    #[test]
    fn bam_zoomed_out_bars_and_hidden_reads() {
        let (mut app, _dir) = with_bam();
        sized(&mut app, 80, 12);
        let terminal = render(&app, 80, 12);
        insta::assert_snapshot!(terminal.backend());
        let screen = rows(&terminal);
        assert!(screen.iter().any(|r| r.contains("━━▸")), "{screen:#?}");
        assert!(screen.iter().any(|r| r.contains("◂━━")), "{screen:#?}");

        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:big\tLN:1000000\n\
                   a\t0\tbig\t1\t60\t10M\t*\t0\t0\t*\t*\n";
        let (app, _dir) = with_sam(sam);
        let screen = rows(&render(&app, 80, 14));
        assert!(
            screen.iter().any(|r| r.contains("zoom in to see reads")),
            "{screen:#?}"
        );
    }

    #[test]
    fn insertions_and_mapq_0_are_styled() {
        let sam = format!(
            "{HEAD}i\t0\tc\t1\t60\t3M2I3M\t*\t0\t0\tACGTTACG\t*\n\
             z\t0\tc\t1\t0\t8M\t*\t0\t0\tACGTACGT\t*\n"
        );
        let (mut app, _dir) = with_sam(&sam);
        base_level_at(&mut app, 1);
        let terminal = render(&app, 80, 14);
        let buffer = terminal.backend().buffer();
        // Reads start on screen row 5: i on the first row, z on the second.
        assert_eq!(buffer[(3, 5)].bg, palette::INSERTION.bg.unwrap());
        assert_ne!(buffer[(2, 5)].bg, palette::INSERTION.bg.unwrap());
        assert!(
            buffer[(0, 6)]
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
        assert!(
            !buffer[(0, 5)]
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
    }

    #[test]
    fn rows_past_the_limit_are_counted() {
        let mut sam = HEAD.to_string();
        for i in 0..1005 {
            sam.push_str(&format!("r{i}\t0\tc\t1\t60\t5M\t*\t0\t0\t*\t*\n"));
        }
        let (mut app, _dir) = with_sam(&sam);
        base_level_at(&mut app, 1);
        let id = id(&app);
        let view = app.bams.get_mut(&id).unwrap();
        assert_eq!((view.packed.rows.len(), view.packed.not_drawn), (1000, 5));
        view.top = 1000 + 1 - view.height;
        let screen = rows(&render(&app, 80, 14));
        assert_eq!(screen[10], "+5 reads not drawn");
    }

    #[test]
    fn mismatches_come_from_md_without_a_reference() {
        let (app, _dir) = bam_at(101, false);
        let screen = rows(&render(&app, 80, 14));
        assert!(screen[5].starts_with(".........G"), "{}", screen[5]);
        assert!(!screen[12].contains("no reference"), "{}", screen[12]);

        let sam = format!("{HEAD}a\t0\tc\t11\t60\t5M\t*\t0\t0\tACGTA\t*\n");
        let (mut app, _dir) = with_sam(&sam);
        base_level_at(&mut app, 1);
        let screen = rows(&render(&app, 80, 14));
        assert!(
            screen[12].contains("no reference (bam ref FILE)"),
            "{}",
            screen[12]
        );
    }

    #[test]
    fn seq_star_reads_and_tiny_terminals_do_not_panic() {
        let (mut app, _dir) = bam_at(701, true);
        let id = id(&app);
        let view = app.bams.get_mut(&id).unwrap();
        view.show_filtered = true;
        view.repack();
        let screen = rows(&render(&app, 80, 14));
        assert_eq!(
            screen[6],
            format!("{:10}{}", "", ".".repeat(40)),
            "r4 has no SEQ"
        );
        for (w, h) in [(20, 5), (1, 1), (80, 3), (0, 0)] {
            sized(&mut app, w, h);
            render(&app, w, h);
        }
    }

    #[test]
    fn small_areas_drop_the_details_before_the_reads() {
        let view = crate::bam_view::BamView::new(0, 1000, 80);
        let layout = bam_layout(Rect::new(0, 0, 80, 3), &view);
        assert!(layout.reads.height >= 1);
        assert_eq!(layout.details.height, 0);
        let layout = bam_layout(Rect::new(0, 0, 80, 10), &view);
        assert_eq!(
            [
                layout.strip,
                layout.ruler,
                layout.reference,
                layout.coverage,
                layout.details
            ]
            .map(|r| r.height),
            [1, 1, 0, 1, 1],
            "no reference row when zoomed out"
        );
        assert_eq!(layout.reads.height, 6);
    }

    #[test]
    fn the_contig_list() {
        let (mut app, _dir) = with_bam();
        sized(&mut app, 60, 14);
        app.handle_event(&ratatui::crossterm::event::Event::Key(
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char('c'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
        ));
        let terminal = render(&app, 60, 14);
        insta::assert_snapshot!(terminal.backend());
        let screen = rows(&terminal);
        assert!(
            screen
                .iter()
                .any(|r| r.contains("2 contigs (1 with no reads hidden; Ctrl-A shows them)")),
            "{screen:#?}"
        );
    }

    #[test]
    fn the_coverage_row_uses_the_sketch_until_the_pass_is_done() {
        let (app, _dir) = with_bam();
        let (dataset, doc, view) = app.bam().unwrap();
        let source = doc.source();
        let mut partial = app.passes[&(dataset.id, 0)].clone();
        partial.done_to = 100;
        partial.bins.iter_mut().for_each(|b| *b = 0.0);
        let mut view = view.clone();
        view.loaded = None;
        let sketched = column_depths(&view, 1000, None, source);
        assert!(sketched.iter().any(|&d| d > 0.0));
        assert_eq!(column_depths(&view, 1000, Some(&partial), source), sketched);
    }

    #[test]
    fn a_stretch_is_highlighted_and_named_on_the_bottom_row() {
        let (mut app, _dir) = bam_at(101, true);
        let id = id(&app);
        let v = app.bams.get_mut(&id).unwrap();
        v.cursor = 120;
        v.marked = Some((113, 131));
        let terminal = render(&app, 80, 14);
        let buf = terminal.backend().buffer();
        for y in [2u16, 3] {
            for x in 0..40u16 {
                let bg = buf[(x, y)].bg;
                if x == 19 && y == 2 {
                    assert_eq!(bg, palette::SELECTED.bg.unwrap(), "the cursor wins");
                } else if (12..=30).contains(&x) {
                    assert_eq!(bg, palette::STRETCH.bg.unwrap(), "{x},{y}");
                } else {
                    assert_ne!(bg, palette::STRETCH.bg.unwrap(), "{x},{y}");
                }
            }
        }
        let screen = rows(&terminal);
        assert_eq!(
            screen[12].trim_end(),
            "c1:113-131  19 bp  E reference  e reads over it  Esc clear"
        );
        let v = app.bams.get_mut(&id).unwrap();
        v.marked = None;
        v.marking = Some(113);
        let screen = rows(&render(&app, 80, 14));
        assert_eq!(
            screen[12].trim_end(),
            "c1:113-120  8 bp  v ends the stretch  Esc clear"
        );
    }

    fn length_of(app: &App) -> u64 {
        let (_, doc, view) = app.bam().unwrap();
        doc.source().contigs()[view.contig].length
    }

    #[test]
    fn depth_under_the_cursor_is_a_number() {
        let (mut app, _dir) = bam_at(101, true);
        let id = id(&app);
        for (pos, depth) in [(101, "depth 1"), (125, "depth 2"), (153, "depth 0")] {
            app.bams.get_mut(&id).unwrap().cursor = pos;
            let (_, _, view) = app.bam().unwrap();
            assert_eq!(
                depth_text(view, length_of(&app)).as_deref(),
                Some(depth),
                "{pos}"
            );
        }
        app.bams.get_mut(&id).unwrap().cursor = 125;
        let screen = rows(&render(&app, 80, 14));
        assert!(
            screen[12].starts_with("c1:125  depth 2  1 bp a column"),
            "{}",
            screen[12]
        );
    }

    #[test]
    fn zoomed_out_the_depth_is_a_mean() {
        let (mut app, _dir) = with_bam();
        sized(&mut app, 80, 14);
        let id = id(&app);
        let v = app.bams.get_mut(&id).unwrap();
        v.per_col = 10;
        v.place(0, 125, 1000);
        settle(&mut app);
        let (_, _, view) = app.bam().unwrap();
        let text = depth_text(view, length_of(&app)).unwrap();
        let number = text.strip_prefix("mean depth ").expect(&text);
        assert_eq!(number.split('.').nth(1).map(str::len), Some(1), "{text}");
    }

    #[test]
    fn selected_reads_say_what_to_do_with_them() {
        let (mut app, _dir) = bam_at(101, true);
        let id = id(&app);
        let r = app.bams[&id].reads().to_vec();
        let v = app.bams.get_mut(&id).unwrap();
        v.select(crate::bam_view::ReadId::of(&r[0]));
        let screen = rows(&render(&app, 120, 14));
        assert!(
            screen[12].contains("1 read selected  e export  Esc clear"),
            "{}",
            screen[12]
        );
        let v = app.bams.get_mut(&id).unwrap();
        v.select(crate::bam_view::ReadId::of(&r[1]));
        let screen = rows(&render(&app, 120, 14));
        assert!(
            screen[12].contains("2 reads selected  e export  Esc clear"),
            "{}",
            screen[12]
        );
    }

    #[test]
    fn no_depth_is_shown_until_the_reads_are_read() {
        let (mut app, _dir) = bam_at(101, true);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().place(1, 1, 500);
        let (_, _, view) = app.bam().unwrap();
        assert_eq!(depth_text(view, 500), None);
        let screen = rows(&render(&app, 80, 14));
        assert!(!screen[12].contains("depth"), "{}", screen[12]);
    }

    #[test]
    fn the_no_reference_note_keeps_its_advice_on_80_columns() {
        let (mut app, _dir) = crate::app::bam::tests::with_multi();
        sized(&mut app, 80, 14);
        crate::app::bam::tests::at_base_level(&mut app, 0, 101, 0);
        app.status.clear();
        let screen = rows(&render(&app, 80, 14));
        assert!(screen[12].ends_with("(bam ref FILE)"), "{}", screen[12]);
    }

    /// The reads heights of a stack layout's members.
    fn reads_heights(l: &StackLayout) -> Vec<u16> {
        l.members.iter().map(|m| m.reads.height).collect()
    }

    /// Every rect of `l` lies inside `area`, and the members' rows do not
    /// overlap one another or the shared rows.
    fn assert_fits(l: &StackLayout, area: Rect) {
        let mut rects = vec![l.strip, l.ruler, l.reference, l.details];
        for m in &l.members {
            rects.extend([m.name, m.coverage, m.reads]);
        }
        let shown: Vec<Rect> = rects.into_iter().filter(|r| r.height > 0).collect();
        for (i, r) in shown.iter().enumerate() {
            assert!(
                area.contains(r.as_position()) && r.bottom() <= area.bottom(),
                "{r:?}"
            );
            for other in &shown[i + 1..] {
                assert!(!r.intersects(*other), "{r:?} overlaps {other:?}");
            }
        }
    }

    #[test]
    fn a_stack_shares_rows_out_by_need() {
        let area = Rect::new(0, 0, 80, 20);
        let l = stack_layout(area, true, &[30, 2]);
        assert_eq!(
            (l.strip.y, l.ruler.y, l.reference.y, l.details.y),
            (0, 1, 2, 19)
        );
        let m = &l.members;
        assert_eq!((m[0].name.y, m[0].coverage.y, m[0].reads.y), (3, 4, 5));
        assert_eq!((m[1].name.y, m[1].coverage.y, m[1].reads.y), (15, 16, 17));
        assert_eq!(reads_heights(&l), [10, 2]);
        assert_fits(&l, area);

        let l = stack_layout(area, true, &[usize::MAX, usize::MAX, 1]);
        assert_eq!(reads_heights(&l), [5, 4, 1]);
        assert_fits(&l, area);

        // Rows left once every need is met are split evenly.
        let l = stack_layout(area, false, &[1, 1]);
        assert_eq!(l.reference.height, 0);
        assert_eq!(reads_heights(&l), [7, 6]);
        assert_fits(&l, area);

        let member = l.member(1);
        assert_eq!(
            (member.strip, member.ruler, member.reference, member.details),
            (l.strip, l.ruler, l.reference, l.details)
        );
        assert_eq!(
            (member.coverage, member.reads),
            (l.members[1].coverage, l.members[1].reads)
        );
    }

    #[test]
    fn a_short_stack_drops_shared_rows_then_coverage() {
        let unknown = [usize::MAX; 3];
        let area = Rect::new(0, 0, 80, 13);
        let l = stack_layout(area, true, &unknown);
        assert_eq!(
            (l.strip.y, l.ruler.y, l.reference.y, l.details.y),
            (0, 1, 2, 12)
        );
        assert_eq!(reads_heights(&l), [1, 1, 1]);
        assert!(l.members.iter().all(|m| m.coverage.height == 1));
        assert_fits(&l, area);

        let area = Rect::new(0, 0, 80, 10);
        let l = stack_layout(area, true, &unknown);
        assert_eq!(
            (
                l.strip.height,
                l.ruler.height,
                l.reference.height,
                l.details.height
            ),
            (0, 1, 0, 0)
        );
        assert_eq!(l.members[0].name.y, 1);
        assert_eq!(reads_heights(&l), [1, 1, 1]);
        assert_fits(&l, area);

        let area = Rect::new(0, 0, 80, 8);
        let l = stack_layout(area, true, &unknown);
        assert_eq!(l.ruler.height, 0);
        assert!(l.members.iter().all(|m| m.coverage.height == 0));
        assert_eq!(reads_heights(&l), [2, 2, 1]);
        assert_eq!(
            l.members.iter().map(|m| m.name.y).collect::<Vec<_>>(),
            [0, 3, 6]
        );
        assert_fits(&l, area);

        let area = Rect::new(0, 0, 80, 6);
        let l = stack_layout(area, true, &[0, 5, 9]);
        assert_eq!(reads_heights(&l), [1, 1, 1]);
        assert!(l.members.iter().all(|m| m.name.height == 1));
        assert_fits(&l, area);

        // Too short even for that: rows stop at the bottom, nothing past it.
        let area = Rect::new(0, 0, 80, 4);
        assert_fits(&stack_layout(area, true, &unknown), area);
    }
}
