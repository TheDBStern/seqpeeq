//! Drawing a read along its own bases, and the list of reads in view
//! that align elsewhere.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use seqpeeq_core::bam::{Contig, grouped};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::bam_view::{ReadKind, cigar_text};
use crate::elsewhere::{ElsewhereList, Group, Via};
use crate::palette;
use crate::read_axis::{AxisLine, AxisView, Found, Kind, Placement, Segment};
use crate::ui::bam::nice;

/// Where the parts of the axis view go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisLayout {
    pub header: Rect,
    /// The labels' column and the bars beside it, as tall as the lines.
    pub labels: Rect,
    pub bars: Rect,
    pub details: Rect,
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `text` cut or padded with spaces to `width` characters.
fn fit(text: &str, width: usize) -> String {
    let mut out: String = text.chars().take(width).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width - n));
    out
}

fn strand(reverse: bool) -> char {
    if reverse { '−' } else { '+' }
}

fn place(contigs: &[Contig], contig: usize, pos: u64, reverse: bool) -> String {
    let name = contigs.get(contig).map_or("?", |c| c.name.as_str());
    format!("{name}:{} ({})", grouped(pos), strand(reverse))
}

/// Each placement's label, in drawing order, its parts lined up.
fn labels(axis: &AxisView, contigs: &[Contig]) -> Vec<String> {
    let placements = axis.placements();
    let places: Vec<String> = placements
        .iter()
        .map(|(_, p)| place(contigs, p.contig, p.pos, p.reverse))
        .collect();
    let mapqs: Vec<String> = placements
        .iter()
        .map(|(_, p)| match p.mapq {
            Some(q) => format!("MAPQ {q}"),
            None => "MAPQ —".to_string(),
        })
        .collect();
    let wide = |v: &[String]| v.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let (pw, mw) = (wide(&places), wide(&mapqs));
    placements
        .iter()
        .zip(places.iter().zip(&mapqs))
        .map(|((_, p), (pl, mq))| format!("{}  {}  {}", fit(pl, pw), fit(mq, mw), p.kind.word()))
        .collect()
}

/// Lay the axis view out in `area`.
pub fn axis_layout(area: Rect, axis: &AxisView, contigs: &[Contig]) -> AxisLayout {
    let header = Rect {
        height: area.height.min(1),
        ..area
    };
    let details_height = u16::from(area.height >= 3);
    let details = Rect {
        y: area.bottom().saturating_sub(details_height),
        height: details_height,
        ..area
    };
    let lines_height = area.height - header.height - details.height;
    let widest = labels(axis, contigs)
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .max("read 1".len()) as u16
        + 2;
    let label_width = widest.min(area.width / 2);
    let labels = Rect {
        y: area.y + header.height,
        width: label_width,
        height: lines_height,
        ..area
    };
    let bars = Rect {
        x: area.x + label_width,
        y: labels.y,
        width: area.width - label_width,
        height: lines_height,
    };
    AxisLayout {
        header,
        labels,
        bars,
        details,
    }
}

/// The header line of the axis view.
pub fn axis_header(app: &App, axis: &AxisView) -> String {
    let n = axis
        .placements()
        .iter()
        .filter(|(_, p)| p.kind != Kind::Unmapped)
        .count();
    let length = |number: u8| {
        axis.segments
            .iter()
            .find(|s| s.number == number)
            .filter(|s| s.length > 0)
            .map_or("?".to_string(), |s| grouped(s.length))
    };
    let alignments = plural(n, "alignment", "alignments");
    let mut text = if axis.segments.iter().any(|s| s.number > 0) {
        format!(
            "{} · pair · read 1 {} bp, read 2 {} bp · {alignments}",
            axis.name,
            length(1),
            length(2)
        )
    } else {
        format!("{} · {} bp · {alignments}", axis.name, length(0))
    };
    let searching = app
        .bam()
        .and_then(|(d, _, _)| app.searches.get(&d.id))
        .is_some_and(|s| {
            s.running
                .as_ref()
                .is_some_and(|(names, _)| names.contains(&axis.name))
        });
    if searching {
        text.push_str(" · searching…");
    } else if !axis.searched {
        text.push_str(" · secondaries not searched · S searches the BAM");
    }
    text
}

fn found_word(found: Found) -> &'static str {
    match found {
        Found::Record => "record",
        Found::Search => "search",
        Found::Sa => "SA",
        Found::Xa => "XA",
    }
}

/// The details of the picked placement.
pub fn axis_details(axis: &AxisView, contigs: &[Contig]) -> String {
    let Some((number, p)) = axis.current() else {
        return String::new();
    };
    let segment = axis.segments.iter().find(|s| s.number == number);
    let mut parts = vec![
        p.kind.word().to_string(),
        place(contigs, p.contig, p.pos, p.reverse),
    ];
    if !p.cigar.is_empty() {
        parts.push(cigar_text(&p.cigar));
    }
    if let Some(nm) = p.nm {
        parts.push(format!("NM {nm}"));
    }
    if let Some(score) = p.score {
        parts.push(format!("AS {score}"));
    }
    if let Some(xs) = segment.and_then(|s| s.suboptimal) {
        parts.push(format!("XS {xs}"));
    }
    parts.push(found_word(p.found).to_string());
    if let Some(k) = segment.map(|s| s.alts_skipped).filter(|&k| k > 0) {
        parts.push(format!(
            "{} unreadable",
            plural(k, "XA entry", "XA entries")
        ));
    }
    parts.join(" · ")
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(ch).set_style(style);
    }
}

/// The read bases column `col` covers.
fn col_span(axis: &AxisView, col: u16) -> (u64, u64) {
    let a = axis.left + u64::from(col) * axis.per_col;
    (a, a + axis.per_col - 1)
}

/// A segment's axis: the positions along the read, its unaligned stretches
/// shaded.
fn draw_axis_line(buf: &mut Buffer, bars: Rect, y: u16, axis: &AxisView, segment: &Segment) {
    if segment.length == 0 {
        buf.set_stringn(bars.x, y, "no bases known", bars.width as usize, dim());
        return;
    }
    let end = segment.length;
    let spacing = nice(axis.per_col * 10);
    let mut labels = vec![axis.left];
    let mut tick = axis.left.div_ceil(spacing) * spacing;
    while tick <= end {
        if tick > axis.left {
            labels.push(tick);
        }
        tick += spacing;
    }
    let mut free = 0u16;
    for pos in labels.into_iter().filter(|&p| p <= end) {
        let Some(col) = axis.col_of(pos) else {
            continue;
        };
        let col = col as u16;
        let text = grouped(pos);
        if col < free || col + text.len() as u16 > bars.width {
            continue;
        }
        buf.set_string(bars.x + col, y, &text, Style::new());
        free = col + text.len() as u16 + 1;
    }
    let uncovered = segment.uncovered();
    for col in 0..bars.width {
        let (a, b) = col_span(axis, col);
        if a > end {
            break;
        }
        let b = b.min(end);
        if uncovered.iter().any(|&(s, e)| s <= a && e >= b)
            && let Some(cell) = buf.cell_mut(Position::new(bars.x + col, y))
        {
            cell.set_style(palette::UNALIGNED);
        }
    }
}

fn kind_style(p: &Placement) -> Style {
    let style = match p.kind {
        Kind::Supplementary => palette::read(ReadKind::Split),
        Kind::Secondary | Kind::Alternative => dim(),
        _ => Style::new(),
    };
    if p.mapq == Some(0) {
        style.add_modifier(Modifier::DIM)
    } else {
        style
    }
}

/// A placement's bar under the stretch of the read it aligns.
fn draw_bar(buf: &mut Buffer, bars: Rect, y: u16, axis: &AxisView, p: &Placement) {
    let Some((a, b)) = p.span else {
        if p.kind == Kind::Unmapped {
            buf.set_stringn(bars.x, y, "unmapped", bars.width as usize, dim());
        }
        return;
    };
    let style = kind_style(p);
    for col in 0..bars.width {
        let (s, e) = col_span(axis, col);
        if s > b {
            break;
        }
        if e < a {
            continue;
        }
        let tip = if p.reverse { s <= a } else { e >= b };
        let ch = match (tip, p.reverse) {
            (true, false) => '▸',
            (true, true) => '◂',
            _ => '━',
        };
        put(buf, bars.x + col, y, ch, style);
    }
}

/// `a`: the read along its own bases, a bar per placement.
/// `text`, then `right` ending at `width` when two spaces or more fit
/// between them; `text` alone when they do not.
fn with_hint(text: String, right: &str, width: usize) -> String {
    let gap = width.saturating_sub(text.width() + right.width());
    if text.width() + right.width() + 2 > width {
        return text;
    }
    format!("{text}{}{right}", " ".repeat(gap))
}

pub fn draw_axis(frame: &mut Frame, app: &App, axis: &AxisView, area: Rect) {
    let Some((_, doc, _)) = app.bam() else {
        return;
    };
    let contigs = doc.source().contigs();
    let layout = axis_layout(area, axis, contigs);
    let buf = frame.buffer_mut();
    let width = area.width as usize;
    if layout.header.height > 0 {
        let header = with_hint(axis_header(app, axis), "Esc back", width);
        buf.set_stringn(area.x, area.y, header, width, Style::new());
    }
    let labels = labels(axis, contigs);
    let lw = layout.labels.width as usize;
    let lines = axis.lines();
    let top = axis.top.min(lines.len().saturating_sub(1));
    for (row, line) in lines.iter().skip(top).enumerate() {
        if row as u16 >= layout.labels.height {
            break;
        }
        let y = layout.labels.y + row as u16;
        match *line {
            AxisLine::Axis(s) => {
                let segment = &axis.segments[s];
                let name = match segment.number {
                    0 => "read".to_string(),
                    n => format!("read {n}"),
                };
                buf.set_stringn(area.x, y, name, lw, dim());
                draw_axis_line(buf, layout.bars, y, axis, segment);
            }
            AxisLine::Placement(s, i, n) => {
                let p = &axis.segments[s].placements[i];
                let style = if n == axis.picked {
                    palette::SELECTED
                } else {
                    Style::new()
                };
                buf.set_stringn(area.x, y, fit(&labels[n], lw), lw, style);
                draw_bar(buf, layout.bars, y, axis, p);
            }
        }
    }
    if layout.details.height > 0 {
        buf.set_stringn(
            area.x,
            layout.details.y,
            axis_details(axis, contigs),
            width,
            dim(),
        );
    }
}

fn via_word(via: Via) -> &'static str {
    match via {
        Via::Split => "split",
        Via::Mate => "mate",
        Via::Alternative => "XA",
        Via::Secondary => "secondary",
        Via::Search => "search",
    }
}

/// How a group's reads get there, counted.
fn kinds(group: &Group) -> String {
    Via::ALL
        .iter()
        .filter(|&&v| group.vias[v as usize] > 0)
        .map(|&v| {
            let n = group.vias[v as usize];
            match v {
                Via::Split => format!("{n} split"),
                Via::Mate => plural(n, "mate", "mates"),
                Via::Alternative => format!("{n} XA"),
                Via::Secondary => plural(n, "secondary", "secondaries"),
                Via::Search => format!("{n} found by search"),
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Where a group's reads go.
fn whereto(list: &ElsewhereList, group: &Group, contigs: &[Contig]) -> String {
    let Some(contig) = group.contig else {
        return "?".to_string();
    };
    let name = contigs.get(contig).map_or("?", |c| c.name.as_str());
    let (a, b) = group.span;
    let mut text = if a == b {
        format!("{name}:{}", grouped(a))
    } else {
        format!("{name}:{}-{}", grouped(a), grouped(b))
    };
    if list.basis.as_ref().is_some_and(|b| b.view.contig == contig) {
        text.push_str(" (same contig)");
    }
    text
}

/// The text of the `rows` the list shows (indexes as `rows()` gives).
fn list_rows(list: &ElsewhereList, contigs: &[Contig], rows: &[usize]) -> Vec<String> {
    match list.open.and_then(|i| list.groups.get(i)) {
        None => {
            let wheres: Vec<String> = rows
                .iter()
                .map(|&i| whereto(list, &list.groups[i], contigs))
                .collect();
            let ww = wheres.iter().map(|w| w.width()).max().unwrap_or(0).min(40);
            rows.iter()
                .zip(&wheres)
                .map(|(&i, w)| {
                    let g = &list.groups[i];
                    let reads = plural(g.reads.len(), "read", "reads");
                    let mut tail = kinds(g);
                    if g.contig.is_none() {
                        let end = if g.pending > 0 {
                            "not yet found · S searches the BAM"
                        } else {
                            "no other placement found outside this stretch"
                        };
                        if !tail.is_empty() {
                            tail.push_str(" · ");
                        }
                        tail.push_str(end);
                    }
                    format!("  {}  {reads:>9}   {tail}", fit(w, ww))
                })
                .collect()
        }
        Some(group) => {
            let nw = group
                .reads
                .iter()
                .map(|r| r.name.width())
                .max()
                .unwrap_or(0)
                .min(30);
            let name_of = |c: usize| contigs.get(c).map_or("?", |c| c.name.as_str());
            rows.iter()
                .map(|&i| {
                    let r = &group.reads[i];
                    let here = format!(
                        "{}:{}-{}",
                        name_of(r.contig),
                        grouped(r.start),
                        grouped(r.end)
                    );
                    let there = match group.contig {
                        Some(c) => group.whither[i]
                            .iter()
                            .map(|&(pos, via)| {
                                format!("{}:{} {}", name_of(c), grouped(pos), via_word(via))
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                        None if group.pending_names.contains(&r.name) => {
                            "? not yet found".to_string()
                        }
                        None => "? no other placement found outside this stretch".to_string(),
                    };
                    format!("  {}  {here}  → {there}", fit(&r.name, nw))
                })
                .collect()
        }
    }
}

/// `A`: the reads in view that align elsewhere, by where.
pub fn draw_elsewhere(frame: &mut Frame, app: &App, list: &ElsewhereList, area: Rect) {
    let Some((_, doc, _)) = app.bam() else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    let contigs = doc.source().contigs();
    let width = area.width as usize;
    let buf = frame.buffer_mut();
    let mut title = match list.open.and_then(|i| list.groups.get(i)) {
        Some(g) => format!(
            "reads in view aligning elsewhere › {}",
            whereto(list, g, contigs)
        ),
        None => "reads in view aligning elsewhere".to_string(),
    };
    let count = grouped(list.read_count as u64);
    let hinted = format!("{count}  Esc back");
    let count = if title.width() + 1 + hinted.width() <= width {
        hinted
    } else {
        count
    };
    let gap = width.saturating_sub(title.width() + count.len());
    title.push_str(&" ".repeat(gap.max(1)));
    title.push_str(&count);
    buf.set_stringn(area.x, area.y, title, width, Style::new());
    let mut y = area.y + 1;
    if (list.typing || !list.filter.text().is_empty()) && y < area.bottom() {
        let lead = "filter: ";
        buf.set_stringn(
            area.x,
            y,
            format!("{lead}{}", list.filter.text()),
            width,
            dim(),
        );
        if list.typing {
            let x =
                area.x + (lead.len() + list.filter.text()[..list.filter.cursor()].width()) as u16;
            if x < area.right() {
                frame.set_cursor_position(Position::new(x, y));
            }
        }
        y += 1;
    }
    let buf = frame.buffer_mut();
    let height = area.bottom().saturating_sub(y) as usize;
    if height == 0 {
        return;
    }
    let mut top = list.top;
    if list.cursor >= top + height {
        top = list.cursor + 1 - height;
    }
    let rows = list.rows();
    let shown = &rows[top.min(rows.len())..(top + height).min(rows.len())];
    for (i, text) in list_rows(list, contigs, shown).iter().enumerate() {
        let style = if top + i == list.cursor {
            palette::SELECTED
        } else {
            Style::new()
        };
        buf.set_stringn(area.x, y, fit(text, width), width, style);
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::app::bam::tests::{at_base_level, id, keys, press, with_multi};
    use crate::bam_view::Over;
    use crate::read_axis::{AxisView, assemble};
    use crate::ui::tests::{render, rows, settle};
    use ratatui::crossterm::event::KeyCode;
    use ratatui::layout::Size;
    use seqpeeq_core::bam::{Read, parse_cigar};

    fn sized(app: &mut App, width: u16, height: u16) {
        app.resize(Size::new(width, height));
        settle(app);
        app.status.clear();
    }

    #[test]
    fn axis_of_a_bwa_pair() {
        let (mut app, _dir) = with_multi();
        sized(&mut app, 80, 14);
        at_base_level(&mut app, 0, 101, 0);
        keys(&mut app, "a");
        app.status.clear();
        let terminal = render(&app, 80, 14);
        insta::assert_snapshot!(terminal.backend());
        let screen = rows(&terminal);
        assert!(
            screen[1].starts_with(
                "x1 · pair · read 1 50 bp, read 2 50 bp · 4 alignments · secondaries not searched"
            ),
            "{}",
            screen[1]
        );
        assert!(
            screen
                .iter()
                .any(|r| r.starts_with("g2:301 (+)  MAPQ —   XA"))
        );
        assert!(
            screen
                .iter()
                .any(|r| r.starts_with("g3:501 (−)  MAPQ —   XA"))
        );
        assert!(
            screen
                .iter()
                .any(|r| r.contains("primary · g1:101 (+) · 50M · NM 0 · AS 50 · XS 50 · record"))
        );
    }

    #[test]
    fn axis_of_a_split_read() {
        let (mut app, _dir) = with_multi();
        sized(&mut app, 80, 10);
        at_base_level(&mut app, 0, 1001, 0);
        keys(&mut app, "a");
        app.status.clear();
        let terminal = render(&app, 80, 10);
        insta::assert_snapshot!(terminal.backend());
        let screen = rows(&terminal);
        assert!(
            screen[1].starts_with("z1 · 100 bp · 2 alignments"),
            "{}",
            screen[1]
        );
    }

    #[test]
    fn reads_aligning_elsewhere() {
        let (mut app, _dir) = with_multi();
        sized(&mut app, 80, 10);
        keys(&mut app, "A");
        let terminal = render(&app, 80, 10);
        insta::assert_snapshot!(terminal.backend());
        let screen = rows(&terminal);
        assert!(screen[1].starts_with("reads in view aligning elsewhere"));
        assert!(screen[1].ends_with("4  Esc back"), "{}", screen[1]);
        assert!(
            screen
                .iter()
                .any(|r| r.contains("not yet found · S searches the BAM"))
        );
        press(&mut app, KeyCode::Enter);
        let screen = rows(&render(&app, 80, 10));
        assert!(
            screen
                .iter()
                .any(|r| r.contains("x1") && r.contains("g2:301 XA")),
            "{screen:#?}"
        );
    }

    #[test]
    fn many_placements_scroll() {
        let (mut app, _dir) = with_multi();
        sized(&mut app, 80, 14);
        let read = |flags: u16, pos: u64| Read {
            name: "q".to_string(),
            flags,
            contig: 0,
            start: pos,
            end: pos + 99,
            cigar: parse_cigar("100M").unwrap(),
            mapq: 60,
            ..Read::default()
        };
        let primary = read(0, 1000);
        let others: Vec<Read> = (1..=500).map(|pos| read(256, pos)).collect();
        let refs: Vec<&Read> = others.iter().collect();
        let axis = AxisView::new("q", assemble(&[&primary], &refs), 40);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().over.push(Over::Axis(axis));
        settle(&mut app);
        for _ in 0..30 {
            press(&mut app, KeyCode::Char('j'));
        }
        let screen = rows(&render(&app, 80, 14));
        assert!(
            screen.iter().any(|r| r.starts_with("g1:30 (+)")),
            "{screen:#?}"
        );
        assert!(
            screen.iter().all(|r| !r.starts_with("g1:1,000 (+)")),
            "scrolled past the primary"
        );
    }

    #[test]
    fn tiny_terminals_draw_the_overlays() {
        let (mut app, _dir) = with_multi();
        at_base_level(&mut app, 0, 101, 0);
        keys(&mut app, "a");
        for (w, h) in [(1, 1), (20, 5), (80, 3)] {
            sized(&mut app, w, h);
            render(&app, w, h);
        }
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "zA");
        for (w, h) in [(1, 1), (20, 5), (80, 3)] {
            sized(&mut app, w, h);
            render(&app, w, h);
        }
    }

    #[test]
    fn a_crowded_list_draws_quickly_and_says_what_was_searched() {
        use crate::elsewhere::ElsewhereList;
        use seqpeeq_core::bam::Region;
        use std::collections::{HashMap, HashSet};
        let (mut app, _dir) = with_multi();
        sized(&mut app, 80, 10);
        let r = Region {
            contig: 0,
            start: 1,
            end: 2000,
        };
        let reads: Vec<Read> = (0..100_000u64)
            .map(|i| Read {
                name: format!("q{i}"),
                start: 1 + i % 1000,
                end: 100 + i % 1000,
                ..Read::default()
            })
            .collect();
        let searched: HashSet<String> = HashSet::from(["q1".to_string()]);
        let mut list = ElsewhereList::build(reads, r, r, &HashMap::new(), &searched);
        list.open = Some(0);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().over.push(Over::List(list));
        render(&app, 80, 10);
        let started = std::time::Instant::now();
        let screen = rows(&render(&app, 80, 10));
        assert!(
            started.elapsed().as_millis() < 100,
            "{:?}",
            started.elapsed()
        );
        assert!(
            screen[1].trim_end().ends_with("100,000  Esc back"),
            "{}",
            screen[1]
        );
        let q1 = screen.iter().find(|r| r.contains("q1 ")).unwrap();
        assert!(
            q1.contains("no other placement found outside this stretch"),
            "{q1}"
        );
    }

    #[test]
    fn the_axis_view_and_the_list_say_esc_goes_back() {
        let (mut app, _dir) = with_multi();
        sized(&mut app, 120, 20);
        at_base_level(&mut app, 0, 1001, 0);
        keys(&mut app, "a");
        let screen = rows(&render(&app, 120, 20));
        assert!(screen[1].ends_with("Esc back"), "{}", screen[1]);
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "A");
        let screen = rows(&render(&app, 120, 20));
        assert!(screen[1].ends_with("  Esc back"), "{}", screen[1]);
        let count = screen[1].trim_end_matches("Esc back").trim_end();
        assert!(
            count.ends_with(|c: char| c.is_ascii_digit()),
            "{}",
            screen[1]
        );
    }

    #[test]
    fn a_narrow_screen_drops_the_hint_first() {
        let (mut app, _dir) = with_multi();
        sized(&mut app, 40, 20);
        at_base_level(&mut app, 0, 1001, 0);
        keys(&mut app, "a");
        let screen = rows(&render(&app, 40, 20));
        assert!(!screen[1].contains("Esc back"), "{}", screen[1]);
        assert!(screen[1].starts_with("z1"), "{}", screen[1]);
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "A");
        let screen = rows(&render(&app, 40, 20));
        assert!(!screen[1].contains("Esc back"), "{}", screen[1]);
    }
}
