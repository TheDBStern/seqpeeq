//! Drawing. Reads the [`App`](crate::app::App) state and never changes it.

pub mod bam;
pub mod grid;
pub mod link;
pub mod overview;
pub mod read_axis;
#[cfg(test)]
mod screens;
#[cfg(test)]
mod svg;
pub mod tree;

use crate::bam_view::Over;
use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Padding, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::msa_view::{Cell, MsaView, Shape};

use crate::app::{App, Confirm, Mode, Typing};
use grid::GridLayout;

/// Rows the log pane takes when shown, borders included.
const LOG_HEIGHT: u16 = 6;

/// The most rows a long status message may take at the bottom.
const MAX_STATUS_ROWS: u16 = 4;

/// Where each part of the screen goes. A part that is not shown has no
/// height.
#[derive(Debug, Clone, Copy)]
pub struct Regions {
    pub tabs: Rect,
    /// The alignment grid, or what stands in for it.
    pub body: Rect,
    pub log: Rect,
    /// One row saying where the cursor is and what is going on.
    pub info: Rect,
    /// The status message, or the line being typed.
    pub bottom: Rect,
}

/// Lay the screen out for the current state. Drawing and mouse handling
/// both use this, so they cannot disagree.
///
/// Rows are handed out by priority, so a small terminal drops pieces instead
/// of overlapping them: bottom line first, then the tab bar, then the body,
/// then the information row, and the log pane only when the body still
/// keeps a row. A status message too long for one row wraps onto up to
/// [`MAX_STATUS_ROWS`], because the end of an error message is where the
/// remedy is.
pub fn regions(app: &App, area: Rect) -> Regions {
    let none = Rect { height: 0, ..area };
    let mut regions = Regions {
        tabs: none,
        body: none,
        log: none,
        info: none,
        bottom: none,
    };
    if area.is_empty() {
        return regions;
    }
    let status_rows = match app.mode {
        Mode::Prompt(_) => 1,
        _ => wrap_columns(&bottom_text(app), area.width as usize).len() as u16,
    };
    let bottom = status_rows
        .clamp(1, MAX_STATUS_ROWS)
        .min(area.height.saturating_sub(2).max(1));
    regions.bottom = Rect {
        y: area.bottom() - bottom,
        height: bottom,
        ..area
    };
    if area.height > bottom {
        regions.tabs = Rect { height: 1, ..area };
    }
    if area.height > bottom + 1 {
        let middle = Rect {
            y: area.y + 1,
            height: area.height - 1 - bottom,
            ..area
        };
        let bam = app
            .session
            .datasets()
            .get(app.active)
            .is_some_and(|d| d.bam().is_some());
        let wanted = app.busy.is_some() || app.alignment().is_some() || app.tree().is_some() || bam;
        let info = u16::from(wanted && middle.height >= 3);
        let log = if app.show_log && middle.height - info > LOG_HEIGHT {
            LOG_HEIGHT
        } else {
            0
        };
        regions.body = Rect {
            height: middle.height - info - log,
            ..middle
        };
        regions.log = Rect {
            y: regions.body.bottom(),
            height: log,
            ..middle
        };
        regions.info = Rect {
            y: regions.log.bottom(),
            height: info,
            ..middle
        };
    }
    regions
}

/// The layout of the alignment on the visible tab as it is shown, the grid
/// or the overview, on a screen of the size the app was last told of.
pub fn grid_of(app: &App) -> Option<GridLayout> {
    let (_, view) = app.alignment()?;
    layout_with(app, score_width(view))
}

/// The cells the overview has, which the zoom levels are fitted to.
pub fn overview_shape(app: &App) -> Option<Shape> {
    layout_with(app, grid::SCORE_WIDTH).map(|layout| grid::screen_shape(&layout))
}

/// The cells the grid has, which decide whether there is an overview.
pub fn plain_shape(app: &App) -> Option<Shape> {
    layout_with(app, 0).map(|layout| grid::screen_shape(&layout))
}

/// The overview gives a column to scores; the grid does not.
fn score_width(view: &MsaView) -> u16 {
    if view.zoom > 0 { grid::SCORE_WIDTH } else { 0 }
}

fn layout_with(app: &App, score_width: u16) -> Option<GridLayout> {
    let (alignment, view) = app.alignment()?;
    let area = Rect::new(0, 0, app.screen.width, app.screen.height);
    let body = regions(app, area).body;
    Some(grid::grid_layout(
        body,
        alignment.rows(),
        view.name_width,
        score_width,
    ))
}

/// Draw the whole interface for the current state.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let regions = regions(app, area);
    draw_bottom(frame, app, regions.bottom);
    if !regions.tabs.is_empty() {
        draw_tabs(frame, app, regions.tabs);
    }
    if !regions.body.is_empty() {
        draw_body(frame, app, regions.body);
    }
    if !regions.log.is_empty() {
        draw_log(frame, app, regions.log);
    }
    if !regions.info.is_empty() {
        draw_info(frame, app, regions.info);
    }
    if matches!(app.mode, Mode::Help) {
        draw_help(frame, app, area);
    }
    if let Mode::CommandHelp(help) = &app.mode {
        draw_command_help(frame, help, area);
    }
    if let Mode::Contigs(list) = &app.mode
        && !regions.body.is_empty()
    {
        bam::draw_contigs(frame, app, list, regions.body);
    }
    if let Mode::Links(list) = &app.mode
        && !regions.body.is_empty()
    {
        link::draw_links(frame, app, list);
    }
}

/// Break text into rows of at most `width` display columns.
fn wrap_columns(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let columns = c.width().unwrap_or(0);
        if used + columns > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("rows is never empty").push(c);
        used += columns;
    }
    rows
}

/// The part of `text` to show in `width` columns so that the cursor, at
/// byte `cursor`, is on screen with one column to spare for itself: the
/// text and the column the cursor is drawn in.
fn window(text: &str, cursor: usize, width: usize) -> (&str, usize) {
    // Start as far left as still shows the cursor: walk back from it, so the
    // cost follows the screen width, not the length of the line.
    let mut start = cursor;
    let mut before = 0;
    for c in text[..cursor].chars().rev() {
        let w = c.width().unwrap_or(0);
        if before + w + 1 > width {
            break;
        }
        before += w;
        start -= c.len_utf8();
    }
    let mut end = start;
    let mut used = 0;
    for c in text[start..].chars() {
        used += c.width().unwrap_or(0);
        if used > width {
            break;
        }
        end += c.len_utf8();
    }
    (&text[start..end], before)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles = tab_labels(app);
    if titles.is_empty() {
        frame.render_widget(Paragraph::new(" seqpeeq").style(dim()), area);
        return;
    }
    let active = active_tab(app);
    let ranges = tab_ranges(&titles, area.width, active);
    for (index, (title, range)) in titles.iter().zip(&ranges).enumerate() {
        if range.is_empty() {
            continue;
        }
        let style = if index == active {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        let tab = Rect {
            x: area.x + range.start,
            width: range.end - range.start,
            ..area
        };
        frame.render_widget(Paragraph::new(format!(" {title} ")).style(style), tab);
        // The column after a tab separates it from the next.
        if range.end < area.width && index + 1 < titles.len() {
            let gap = Rect {
                x: area.x + range.end,
                width: 1,
                ..area
            };
            frame.render_widget(Paragraph::new("│").style(dim()), gap);
        }
    }
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
    if let Some((alignment, view)) = app.alignment() {
        if view.zoom > 0 {
            overview::draw_overview(
                frame,
                alignment,
                view,
                app.conservation(),
                app.summary(),
                area,
            );
        } else {
            grid::draw_grid(frame, alignment, view, app.conservation(), area);
        }
        // The terminal's own cursor marks the cursor cell.
        let layout = grid::grid_layout(area, alignment.rows(), view.name_width, score_width(view));
        if matches!(app.mode, Mode::Normal | Mode::Edit(_))
            && let Some(position) = grid::cursor_position(&layout, view)
        {
            frame.set_cursor_position(position);
        }
        return;
    }
    if let Some((dataset, doc, view)) = app.tree() {
        let name = |d: &seqpeeq_core::Dataset| {
            d.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        let linked = app
            .session
            .link_of(dataset.id)
            .and_then(|id| app.session.datasets().iter().find(|d| d.id == id))
            .map(name);
        let header = tree::header_text(&name(dataset), doc, view, linked.as_deref());
        tree::draw_tree(frame, doc, view, &header, area);
        return;
    }
    if let Some((_, _, view)) = app.bam() {
        match view.over.last() {
            Some(Over::Axis(axis)) => read_axis::draw_axis(frame, app, axis, area),
            Some(Over::List(list)) => read_axis::draw_elsewhere(frame, app, list, area),
            None => match app.session.stack_of(app.session.datasets()[app.active].id) {
                Some(members) => bam::draw_stack(frame, app, members, area),
                None => bam::draw_bam(frame, app, area),
            },
        }
        return;
    }
    let lines = match app.session.datasets().get(app.active) {
        None => vec![Line::from(
            "No files open. :open <path> to open one, ? for help",
        )],
        Some(dataset) => match dataset.tree() {
            Some(_) => Vec::new(),
            None => vec![
                Line::from(dataset.path.display().to_string()),
                Line::from(format!("{}, {} bytes", dataset.kind.as_str(), dataset.size)),
                Line::styled("viewer not built yet", dim()),
            ],
        },
    };
    let block = Block::new().padding(Padding::new(1, 1, 1, 0));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The two halves of the information row: where the cursor is, on the
/// left; what is going on, on the right.
pub fn info_text(app: &App) -> (String, String) {
    let mut left = String::new();
    let mut right = Vec::new();
    if let Some(busy) = &app.busy {
        right.push(job_text(&busy.label, busy.progress.fraction()));
    }
    if let Some((_, _, progress)) = &app.counting {
        right.push(job_text("counting columns", progress.fraction()));
    }
    if let Some((_, _, progress)) = &app.summarising {
        right.push(job_text("summarising", progress.fraction()));
    }
    if let Some((_, _, progress)) = &app.passing {
        right.push(job_text("coverage", progress.fraction()));
    }
    if let Some(reading) = &app.reading {
        right.push(job_text("reading", reading.progress.fraction()));
    }
    if let Some(jumping) = &app.jumping {
        right.push(job_text("jumping", jumping.progress.fraction()));
    }
    for (_, progress) in app.searches.values().filter_map(|s| s.running.as_ref()) {
        right.push(job_text("searching", progress.fraction()));
    }
    if let Some((_, doc, view)) = app.bam() {
        let contig = &doc.source().contigs()[view.contig];
        let depth = bam::depth_text(view, contig.length)
            .map(|d| format!("  {d}"))
            .unwrap_or_default();
        left = format!(
            "{}:{}{depth}  {} bp a column",
            contig.name, view.cursor, view.per_col
        );
        if !view.over.is_empty() {
            // The keys below describe the reference view, which a read-axis
            // view hides; what it offers here is the way back.
            left = "Esc back".to_string();
        } else if let Some((a, b)) = view.stretch() {
            let tail = if view.marking.is_some() {
                "v ends the stretch  Esc clear"
            } else {
                "E reference  e reads over it  Esc clear"
            };
            left = format!(
                "{}:{a}-{b}  {} bp  {tail}",
                contig.name,
                seqpeeq_core::bam::grouped(b - a + 1)
            );
        } else {
            match view.selected.len() {
                0 => {}
                1 => left.push_str("  1 read selected  e export  Esc clear"),
                n => left.push_str(&format!("  {n} reads selected  e export  Esc clear")),
            }
            if view.per_col == 1 && !bam::knows_reference(app) {
                left.push_str("  no reference (bam ref FILE)");
            }
        }
    }
    if let Some((alignment, view)) = app.alignment()
        && view.zoom > 0
    {
        // The real rows and columns under the cursor's cell.
        let span = |at: usize, per: usize, total: usize| {
            let end = (at + per).min(total);
            if end <= at + 1 {
                format!("{}/{total}", at + 1)
            } else {
                format!("{}-{end}/{total}", at + 1)
            }
        };
        let Cell { row, col } = view.cursor;
        let (per_line, per_cell) = (view.scale.rows, view.scale.cols);
        left = format!(
            "rows {} · cols {}",
            span(row, per_line, alignment.rows()),
            span(col, per_cell, alignment.cols())
        );
        if let Some(selected) = selection_text(alignment, view) {
            left.push_str(&format!(" · {selected}"));
        }
        if alignment.is_modified() {
            right.push("unsaved".to_string());
        }
        let plural = |n: usize, noun: &str| {
            if n == 1 {
                format!("1 {noun}")
            } else {
                format!("{n} {noun}s")
            }
        };
        right.push(format!(
            "each cell: {} × {}",
            plural(per_cell, "column"),
            plural(per_line, "sequence")
        ));
    } else if let Some((alignment, view)) = app.alignment() {
        let Cell { row, col } = view.cursor;
        left = format!(
            "row {}/{} · col {}/{}",
            row + 1,
            alignment.rows(),
            col + 1,
            alignment.cols()
        );
        let mut residue = Vec::new();
        if row < alignment.rows() {
            alignment.residues(row, col..col + 1, &mut residue);
        }
        if let Some(&residue) = residue.first() {
            let shown = if residue.is_ascii_graphic() {
                residue as char
            } else {
                '?'
            };
            left.push_str(&format!(" · {shown}"));
        }
        if let Some(selected) = selection_text(alignment, view) {
            left.push_str(&format!(" · {selected}"));
        }
        if alignment.is_modified() {
            right.push("unsaved".to_string());
        }
    }
    if let Some((_, doc, view)) = app.tree() {
        let (tree, facts) = (doc.tree(), doc.facts());
        let n = view.cursor;
        let tips = |k: u32| {
            if k == 1 {
                "1 tip".to_string()
            } else {
                format!("{k} tips")
            }
        };
        let mut parts = Vec::new();
        if tree.is_tip(n) {
            parts.push(tree.label(n).unwrap_or_default().to_string());
        } else {
            let name = |n| tree.label(n).unwrap_or_default();
            parts.push(format!(
                "{}: {} … {}",
                tips(facts.tips(n)),
                name(facts.first_tip(n)),
                name(facts.last_tip(n))
            ));
            if let Some(support) = tree.support(n) {
                parts.push(format!("support {support}"));
            }
        }
        if let Some(length) = tree.length(n) {
            parts.push(format!("length {length}"));
        }
        left = parts.join(" · ");
        match view.picked[..] {
            [] => {}
            [one] => right.push(format!("selected: {}", tips(facts.tips(one)))),
            _ => {
                let count = view.picked_tips(tree).len() as u32;
                right.push(format!("selected: {}", tips(count)));
            }
        }
        if doc.is_modified() {
            right.push("unsaved".to_string());
        }
    }
    match app.mode {
        Mode::Edit(Typing::Insert) => right.push("INSERT".to_string()),
        Mode::Edit(Typing::Replace) => right.push("REPLACE".to_string()),
        _ => {}
    }
    (left, right.join(" · "))
}

/// What is selected, as the information row says it.
fn selection_text(alignment: &seqpeeq_core::Alignment, view: &MsaView) -> Option<String> {
    let (rows, cols) = view.region(Shape::of(alignment))?.flags();
    // Scattered rows or columns are counted too.
    let listed = |what: &str, ranges: &seqpeeq_core::Ranges| match ranges.as_slice().len() {
        1 => format!("{what} {ranges}"),
        _ => format!("{what} {ranges} ({})", ranges.count()),
    };
    Some(match (rows.rows, cols) {
        (Some(rows), Some(cols)) => format!("rows {rows} × cols {cols}"),
        (Some(rows), None) => listed("rows", &rows),
        (None, Some(cols)) => listed("cols", &cols),
        (None, None) => String::new(),
    })
}

/// `opening big.fa 42%`, or the label alone while the total is not known.
pub fn job_text(label: &str, fraction: Option<f64>) -> String {
    match fraction {
        Some(fraction) => format!("{label} {:.0}%", (fraction * 100.0).floor()),
        None => label.to_string(),
    }
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let (left, right) = info_text(app);
    frame.render_widget(Paragraph::new(left), area);
    if right.is_empty() {
        return;
    }
    // The right half wins where the two would meet.
    let width = (right.width() as u16 + 1).min(area.width);
    let at = Rect {
        x: area.right() - width,
        width,
        ..area
    };
    frame.render_widget(Clear, at);
    frame.render_widget(Paragraph::new(format!(" {right}")).style(dim()), at);
}

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title("log").border_style(dim());
    let rows = block.inner(area).height as usize;
    let history = app.session.history();
    let lines: Vec<Line> = if history.is_empty() {
        vec![Line::styled("(no commands yet)", dim())]
    } else {
        let skip = history.len().saturating_sub(rows);
        history[skip..]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect()
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// What the bottom of the screen says when nothing is being typed there:
/// the question being asked, or else the last outcome.
pub(crate) fn bottom_text(app: &App) -> String {
    let Mode::Confirm(confirm) = app.mode else {
        return app.status.clone();
    };
    if let (Confirm::Quit, Some(busy)) = (confirm, &app.busy) {
        return format!(
            "Still {}; quitting stops it. Quit anyway? (y/n)",
            busy.label
        );
    }
    let unwritten = app.unwritten();
    let subject = match (confirm, unwritten.as_slice()) {
        // The job that prompted the question has ended meanwhile.
        (Confirm::Quit, []) => return "Quit? (y/n)".to_string(),
        (Confirm::Quit, [name]) => format!("{name} has edits"),
        (Confirm::Quit, names) => format!("{} files have edits", names.len()),
        (Confirm::Close, _) => {
            let titles = tab_titles(app);
            let name = titles.get(app.active).map_or("this file", String::as_str);
            format!("{name} has edits")
        }
    };
    let question = match confirm {
        Confirm::Quit => "Quit anyway?",
        Confirm::Close => "Close it anyway?",
    };
    format!("{subject} not written to a file. {question} (y/n)")
}

fn draw_bottom(frame: &mut Frame, app: &App, area: Rect) {
    if let Mode::Prompt(prompt) = &app.mode {
        // Show the part of a long line where the typing happens.
        let lead = prompt.kind.lead();
        let text = format!("{lead}{}", prompt.line.text());
        let cursor = lead.len() + prompt.line.cursor();
        let (visible, column) = window(&text, cursor, area.width as usize);
        frame.render_widget(Paragraph::new(visible), area);
        frame.set_cursor_position(Position::new(area.x + column as u16, area.y));
        return;
    }
    let text = bottom_text(app);
    let status = wrap_columns(&text, area.width as usize);
    let shown = status.len().min(area.height as usize);
    let mut lines: Vec<String> = status[..shown].to_vec();
    if shown < status.len()
        && let Some(last) = lines.last_mut()
    {
        last.pop();
        last.push('…');
    }
    let style = if text.starts_with("error:") {
        Style::new().fg(Color::Red)
    } else {
        Style::new()
    };
    let lines: Vec<Line> = lines.into_iter().map(Line::from).collect();
    frame.render_widget(Paragraph::new(lines).style(style), area);
}

/// What the help screen says, above the line about the log. No line is
/// wider than 56 columns, so the box fits a 60-column terminal.
/// The keys of an alignment tab.
const HELP_ALIGNMENT: &[&str] = &[
    "Move      arrows hjkl   Shift+arrows jumps",
    "          PgUp PgDn   Home End   g go to",
    "          / search names   n N next, previous",
    "Select    drag   right-click extends   v V C   Esc clear",
    "          Space, right-click a name or number  pick",
    "Edit      d delete/gap   f fill   e extract   r rename",
    "          many: msa rename --regex RE --replace TEXT",
    "          i insert (shifts)   R type over   Esc stop",
    "Zoom      - out   + in   z view whole alignment",
    "          u U undo redo   w save the whole alignment",
    "          c cycle colors   [ ] adjust name width",
    "          msa delete-rows  delete-cols  fill  overwrite",
    "          msa insert  remove  extract  rename",
];

/// The keys of a tree tab.
const HELP_TREE: &[&str] = &[
    "Tree      jk rows  h parent  l back  Enter open/close",
    "          + - z zoom  b lengths  / n N find  v select",
    "          Space, right-click  pick several clades",
    "          d prune  r rename  o re-root on branch",
    "          O re-root at the midpoint",
    "          many: tree rename --regex RE --replace TEXT",
    "          w save the whole tree   e extract the clade",
    "          s ladderize  u U undo redo",
    "          a select the tips' sequences in the MSA",
    "          p link to an open MSA",
    "          tree extract  prune  reroot  ladderize  rename",
    "          tree link  unlink",
];

/// The keys of a BAM tab.
const HELP_BAM: &[&str] = &[
    "Move      arrows hjkl   Shift+arrows a screen",
    "          PgUp PgDn   Home End   g go to, Tab a contig",
    "BAM       ] [ covered   } { no coverage   c list contigs",
    "          m mate   s other split part   ' back",
    "          - + z zoom  F toggle filtered  S toggle clips",
    "          click, drag, Space select   Esc clear",
    "          Tab next BAM in a stack   :bam stack  unstack",
    "          drag the coordinates, v v  select a region",
    "          e export reads   E export reference",
    "          a visualize all alignments for a read",
    "          A list all alignments for a read",
    "          in those: S search the BAM   g go   / filter",
    "          bam ref export-region export-reads export-ref",
];

/// The keys every tab has, after its own.
const HELP_SHARED: &[&str] = &[
    "View      Tab next file   L log   ? help   q quit",
    "Commands  :open close use undo redo   :CMD -h details",
    "          :rescue FILE if the log has failed",
    "          Tab completes a file name after :tree link",
    "Mouse     wheel over the coordinates scrolls left-right",
    "          hold Shift (Option on macOS) to select text",
    "          with the terminal itself",
    "          in tmux: set -g mouse on",
];

/// The help for the tab in view: its title and lines, its file type's
/// keys first, then the shared ones.
pub fn help_page(app: &App) -> (&'static str, Vec<&'static str>) {
    let dataset = app.session.datasets().get(app.active);
    let (title, own): (&str, &[&str]) = match dataset {
        Some(d) if d.msa().is_some() => ("help: alignment", HELP_ALIGNMENT),
        Some(d) if d.tree().is_some() => ("help: tree", HELP_TREE),
        Some(d) if d.bam().is_some() => ("help: BAM", HELP_BAM),
        _ => ("help", &[]),
    };
    (title, own.iter().chain(HELP_SHARED).copied().collect())
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    // Content plus a border and one column of padding each side.
    let room = area.width.saturating_sub(4) as usize;
    let (title, page) = help_page(app);
    let mut lines: Vec<Line> = page.into_iter().map(Line::from).collect();
    if let Some(path) = app.session.log_path() {
        // The end of a path says which file it is; show that if the whole
        // does not fit.
        let whole = format!("log: {}", path.display());
        let line = if whole.width() <= room {
            whole
        } else {
            let path = path.display().to_string();
            let fits = room.saturating_sub("log: …".width());
            let mut start = path.len();
            let mut used = 0;
            for (index, c) in path.char_indices().rev() {
                used += c.width().unwrap_or(0);
                if used > fits {
                    break;
                }
                start = index;
            }
            format!("log: …{}", &path[start..])
        };
        lines.push(Line::from(line));
    }
    let widest = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (widest + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .title(title)
        .padding(Padding::horizontal(1));
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// `text` in rows of at most `width` columns, broken at spaces where it
/// can be. Rows after the first start with `indent` spaces.
fn wrap_words(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.max(1);
    let indent = if indent < width { indent } else { 0 };
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    // Whether the row holds anything but its indent.
    let mut has_text = false;
    for word in text.split(' ') {
        if has_text && used + 1 + word.width() > width {
            rows.push(std::mem::replace(&mut row, " ".repeat(indent)));
            used = indent;
            has_text = false;
        }
        if has_text {
            row.push(' ');
            used += 1;
        }
        // A word wider than a whole row is broken where the row ends.
        for c in word.chars() {
            let columns = c.width().unwrap_or(0);
            if has_text && used + columns > width {
                rows.push(std::mem::replace(&mut row, " ".repeat(indent)));
                used = indent;
            }
            row.push(c);
            used += columns;
            has_text = true;
        }
    }
    rows.push(row);
    rows
}

/// The box `:CMD -h` opens: the page wrapped to the screen, scrolled.
fn draw_command_help(frame: &mut Frame, help: &crate::app::CommandHelp, area: Rect) {
    if area.width < 5 || area.height < 3 {
        return;
    }
    // A border and one column of padding each side.
    let room = area.width as usize - 4;
    let lines: Vec<String> = help
        .page
        .lines
        .iter()
        .flat_map(|line| {
            // A note's later rows line up under its text, not its dash.
            let indent = if line.starts_with("- ") { 2 } else { 0 };
            wrap_words(line, room, indent)
        })
        .collect();
    let shown = (area.height as usize - 2).min(lines.len());
    let max_scroll = lines.len() - shown;
    help.max_scroll.set(max_scroll);
    help.shown.set(shown);
    let top = help.scroll.min(max_scroll);
    let title_width = help.page.title.width() + 2;
    let widest = lines.iter().map(|l| l.width()).max().unwrap_or(0);
    let width = (widest.max(title_width) + 4).min(area.width as usize) as u16;
    let height = shown as u16 + 2;
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let mut block = Block::bordered()
        .title(help.page.title.as_str())
        .padding(Padding::horizontal(1));
    if max_scroll > 0 {
        block = block.title_bottom("j k scroll  Esc close");
    }
    let text: Vec<Line> = lines[top..top + shown]
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(text).block(block), popup);
}

/// The tab label of each open dataset: its file name.
pub fn tab_titles(app: &App) -> Vec<String> {
    app.session
        .datasets()
        .iter()
        .map(|dataset| match dataset.path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => dataset.path.display().to_string(),
        })
        .collect()
}

/// The open files on each tab, as dataset positions: a stack of BAMs
/// shares one tab, in stack order, at its first member's place; every
/// other file has a tab of its own.
pub fn tab_groups(app: &App) -> Vec<Vec<usize>> {
    let datasets = app.session.datasets();
    let at = |id: &u64| datasets.iter().position(|d| d.id == *id);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, dataset) in datasets.iter().enumerate() {
        match app.session.stack_of(dataset.id) {
            Some(members) => {
                let members: Vec<usize> = members.iter().filter_map(at).collect();
                if members.iter().min() == Some(&index) {
                    groups.push(members);
                }
            }
            None => groups.push(vec![index]),
        }
    }
    groups
}

/// Each tab's label: its file's name, or for a stack its first BAM's
/// name and how many more it holds (`reads.bam +1`).
pub fn tab_labels(app: &App) -> Vec<String> {
    let titles = tab_titles(app);
    tab_groups(app)
        .iter()
        .map(|group| match group.as_slice() {
            [one] => titles[*one].clone(),
            [first, rest @ ..] => format!("{} +{}", titles[*first], rest.len()),
            [] => String::new(),
        })
        .collect()
}

/// The tab holding the file shown.
pub fn active_tab(app: &App) -> usize {
    tab_groups(app)
        .iter()
        .position(|group| group.contains(&app.active))
        .unwrap_or(0)
}

/// The columns each tab occupies in a tab bar `width` columns wide; an
/// empty range for a tab that is not shown. A tab is drawn as its title
/// with one space either side, and tabs are separated by one `│` column.
/// When they do not all fit, the bar starts at a later tab, so that the
/// `active` one is shown whole if it can be.
///
/// Drawing and click handling both use this, so they cannot disagree.
pub fn tab_ranges(titles: &[String], width: u16, active: usize) -> Vec<Range<u16>> {
    let widths: Vec<u32> = titles
        .iter()
        .map(|title| title.width() as u32 + 2)
        .collect();
    // The first tab to draw: the earliest from which everything up to and
    // including the active tab fits.
    let active = active.min(titles.len().saturating_sub(1));
    let mut first = active;
    let mut used = widths.get(active).copied().unwrap_or(0);
    while first > 0 && used + 1 + widths[first - 1] <= u32::from(width) {
        first -= 1;
        used += 1 + widths[first];
    }
    let mut start = 0u32;
    widths
        .iter()
        .enumerate()
        .map(|(index, &tab)| {
            if index < first || start >= u32::from(width) {
                return 0..0;
            }
            let end = (start + tab).min(u32::from(width));
            let range = start as u16..end as u16;
            start = end + 1;
            range
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::{Mode, Prompt, PromptKind};
    use crate::line::LineEdit;
    use crate::msa_view::{Cell, Colouring};
    use crate::palette;
    use crate::ui::grid::grid_layout;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Position;
    use seqpeeq_core::{Command, Session};

    fn terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    /// The `:` line holding `text`, with the cursor at its end.
    fn command_line(text: &str) -> Mode {
        Mode::Prompt(Prompt::new(PromptKind::Command, LineEdit::new(text)))
    }

    pub(crate) fn render(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = terminal(width, height);
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
    }

    /// Do, on this thread, the work the loop would hand to worker threads.
    pub(super) fn settle(app: &mut App) {
        loop {
            app.pump();
            let Some(request) = app.outbox.pop() else {
                break;
            };
            crate::worker::perform(request, &mut |done| app.handle_done(done));
        }
    }

    /// An app with one file open.
    pub(crate) fn with_file(path: &str) -> App {
        let mut app = App::new(Session::new(None));
        app.enqueue(Command::Open {
            path: path.into(),
            kind: None,
        });
        settle(&mut app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
        app
    }

    /// An app with the small fixture alignment open. The path is relative
    /// (tests run in the crate directory), so snapshots hold no
    /// machine-specific text.
    fn with_dataset() -> App {
        with_file("tests/data/aln.fa")
    }

    #[test]
    fn a_tree_tab_draws_the_tree_under_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let (fa, nwk) = (dir.path().join("a.fa"), dir.path().join("t.nwk"));
        std::fs::write(&fa, ">a\nA\n").unwrap();
        std::fs::write(&nwk, "(a,b);").unwrap();
        let mut app = with_file(fa.to_str().unwrap());
        let open = Command::Open {
            path: nwk.clone(),
            kind: None,
        };
        let link = Command::Tree(seqpeeq_core::TreeCmd::Link { file: fa.clone() });
        for cmd in [open, link] {
            app.run_command(cmd).unwrap();
            settle(&mut app);
        }
        let screen = rows(&render(&app, 80, 12)).join("\n");
        assert!(
            screen.contains("t.nwk · 2 tips · linked to a.fa (1 of 2)"),
            "{screen}"
        );
        assert!(screen.contains("─ a") && screen.contains("─ b"), "{screen}");
        assert!(!info_text(&app).1.contains("unsaved"));
        run(&mut app, "tree ladderize --reverse");
        assert!(info_text(&app).1.contains("unsaved"));
    }

    #[test]
    fn empty_state() {
        let app = App::new(Session::new(None));
        insta::assert_snapshot!(render(&app, 60, 12).backend());
    }

    #[test]
    fn one_dataset_with_log() {
        let mut app = with_dataset();
        app.show_log = true;
        insta::assert_snapshot!(render(&app, 60, 12).backend());
    }

    #[test]
    fn command_mode() {
        let mut app = with_dataset();
        app.mode = command_line("open x");
        let mut terminal = render(&app, 60, 12);
        insta::assert_snapshot!(terminal.backend());
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(7, 11)
        );
    }

    #[test]
    fn help_overlay() {
        let mut app = with_dataset();
        app.mode = Mode::Help;
        insta::assert_snapshot!(render(&app, 60, 12).backend());
    }

    /// The box `:CMD -h` opens, for the help `line` asks for.
    fn command_help(line: &str) -> Mode {
        let page = seqpeeq_core::help_request(line).unwrap().unwrap();
        Mode::CommandHelp(crate::app::CommandHelp::new(page))
    }

    #[test]
    fn each_help_page_names_its_commands_and_how_to_get_details() {
        use seqpeeq_core::command::help::ENTRIES;
        for (group, page) in [
            ("msa", HELP_ALIGNMENT),
            ("tree", HELP_TREE),
            ("bam", HELP_BAM),
        ] {
            let words: Vec<&str> = page
                .iter()
                .chain(HELP_SHARED)
                .flat_map(|l| l.split(|c: char| c.is_whitespace() || c == ':'))
                .collect();
            for entry in ENTRIES {
                // "msa fill" → in the msa page as "fill"; "open" → in every page.
                let name = match entry.path.split_once(' ') {
                    Some((g, name)) if g == group => name,
                    Some(_) => continue,
                    None => entry.path,
                };
                assert!(words.contains(&name), "{group} help lacks {:?}", entry.path);
            }
        }
        assert!(HELP_SHARED.iter().any(|l| l.contains("-h")));
    }

    #[test]
    fn command_help_box() {
        let mut app = App::new(Session::new(None));
        app.mode = command_help("msa fill -h");
        insta::assert_snapshot!(render(&app, 60, 24).backend());
    }

    #[test]
    fn a_long_command_help_scrolls_to_its_end_and_no_further() {
        let mut app = App::new(Session::new(None));
        app.mode = command_help("msa rename -h");
        let screen = rows(&render(&app, 60, 12)).join("\n");
        assert!(screen.contains("msa rename [rows]"), "{screen}");
        assert!(!screen.contains("seqpeeq says how many"), "{screen}");
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        for _ in 0..200 {
            app.handle_event(&j);
            render(&app, 60, 12);
        }
        let end = rows(&render(&app, 60, 12)).join("\n");
        assert!(end.contains("seqpeeq says how many"), "{end}");
        assert!(!end.contains("msa rename [rows]"), "{end}");
        let k = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        app.handle_event(&k);
        let up = rows(&render(&app, 60, 12)).join("\n");
        assert_ne!(up, end, "one k after the end moves up a line");
        let home = Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        app.handle_event(&home);
        assert_eq!(rows(&render(&app, 60, 12)).join("\n"), screen);
        let end_key = Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        app.handle_event(&end_key);
        assert_eq!(rows(&render(&app, 60, 12)).join("\n"), end);
    }

    #[test]
    fn command_help_wraps_at_spaces() {
        let mut app = App::new(Session::new(None));
        app.mode = command_help("bam export-reads -h");
        let screen = rows(&render(&app, 60, 24)).join("\n");
        // Every word of the syntax line is on screen whole.
        for word in [
            "[--region",
            "--names-file",
            "[--with-mates]",
            "[--everywhere]",
        ] {
            assert!(screen.contains(word), "{word}\n{screen}");
        }
    }

    #[test]
    fn command_help_draws_on_tiny_screens() {
        let mut app = App::new(Session::new(None));
        app.mode = command_help("help");
        for (width, height) in [(30, 8), (10, 3), (4, 2), (1, 1)] {
            render(&app, width, height);
        }
    }

    /// The text of each screen row, trailing spaces removed.
    pub(crate) fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                row.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn long_status_wraps_so_the_end_is_readable() {
        let mut app = with_dataset();
        app.status = format!(
            "error: cannot write session log {}: Permission denied; use --log PATH or --no-log",
            "/data/project/run".repeat(3)
        );
        let screen = rows(&render(&app, 40, 10));
        let bottom = screen[6..].concat();
        assert!(
            bottom.starts_with("error: cannot write") && bottom.ends_with("or --no-log"),
            "{screen:#?}"
        );
        assert!(
            screen[5].starts_with("row 1/2 · col 1/8"),
            "the information row stays above the status: {screen:#?}"
        );
        assert!(screen[2].contains("seq1"), "{screen:#?}");
    }

    #[test]
    fn status_never_takes_more_than_four_rows() {
        let mut app = with_dataset();
        app.status = "x".repeat(1000);
        let screen = rows(&render(&app, 40, 12));
        assert_eq!(screen.iter().filter(|r| r.starts_with("xxxx")).count(), 4);
        assert_eq!(screen[0], " aln.fa");
    }

    #[test]
    fn long_command_line_shows_its_end() {
        let mut app = with_dataset();
        app.mode = command_line(&format!("open {}END", "/long/path".repeat(5)));
        let mut terminal = render(&app, 20, 6);
        let screen = rows(&terminal);
        assert!(screen[5].ends_with("END"), "{screen:#?}");
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(19, 5)
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut app = with_dataset();
        app.show_log = true;
        for mode in [Mode::Help, command_line("open a very long command line")] {
            app.mode = mode;
            for (width, height) in [(1, 1), (20, 3), (5, 40), (2, 2), (60, 7)] {
                render(&app, width, height);
            }
        }
        let mut app = with_file("tests/data/grid.fa");
        app.show_log = true;
        for (width, height) in [(1, 1), (5, 3), (20, 3), (2, 40), (27, 4), (28, 2), (60, 7)] {
            render(&app, width, height);
        }
    }

    #[test]
    fn tab_ranges_count_wide_characters_by_display_width() {
        // Two double-width characters: 4 columns of title plus the two spaces.
        assert_eq!(tab_ranges(&["配列".to_string()], 80, 0)[0], 0..6);
    }

    #[test]
    fn the_cursor_is_drawn_where_the_typing_is() {
        let mut app = with_dataset();
        let mut line = LineEdit::new("open abc");
        for _ in 0..4 {
            line.key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        app.mode = Mode::Prompt(Prompt::new(PromptKind::Command, line));
        let mut terminal = render(&app, 60, 12);
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(5, 11)
        );
        assert_eq!(rows(&terminal)[11], ":open abc");
        // In a narrow terminal the part around the cursor is shown.
        let mut narrow = render(&app, 6, 4);
        let x = narrow.get_cursor_position().unwrap().x;
        assert!(x < 6);
        assert!(rows(&narrow)[3].contains("en"), "{:?}", rows(&narrow));
    }

    #[test]
    fn alignment_grid() {
        let app = with_file("tests/data/grid.fa");
        insta::assert_snapshot!(render(&app, 70, 9).backend());
    }

    #[test]
    fn the_grid_shows_ruler_numbers_names_and_residues() {
        let app = with_file("tests/data/grid.fa");
        let screen = rows(&render(&app, 70, 9));
        assert_eq!(
            screen[1],
            format!("{:27}1        10        20        30        40", "")
        );
        // Number, name padded to 24 columns, then the 43 residues that fit.
        assert_eq!(
            screen[2],
            format!(
                "1 {:24} {}",
                "Homo_sapiens_A1", "ACGTTGCA--ACGTAGCTAGCATCGNTCGATCGTAGCTAGCTA"
            )
        );
        assert!(
            screen[4].starts_with("3 Gorilla_gorilla_gorilla… ACGTTGGA--"),
            "{}",
            screen[4]
        );
        assert_eq!(screen[7], "row 1/5 · col 1/60 · A");
    }

    fn view_mut(app: &mut App) -> &mut crate::msa_view::MsaView {
        let id = app.session.datasets()[0].id;
        app.views.get_mut(&id).unwrap()
    }

    #[test]
    fn the_ruler_and_cells_scroll_together() {
        let mut app = with_file("tests/data/grid.fa");
        view_mut(&mut app).left = 5;
        let screen = rows(&render(&app, 70, 9));
        assert_eq!(&screen[1][27..33], "    10");
        assert_eq!(&screen[2][27..37], "GCA--ACGTA");
    }

    #[test]
    fn residues_are_coloured_by_what_they_are() {
        let mut app = with_file("tests/data/grid.fa");
        view_mut(&mut app).colouring = Colouring::Residue;
        let terminal = render(&app, 70, 9);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(27, 2)].bg, Color::Indexed(71), "A");
        assert_eq!(buffer[(28, 2)].bg, Color::Indexed(68), "C");
        assert_eq!(buffer[(29, 2)].bg, Color::Indexed(178), "G");
        assert_eq!(buffer[(30, 2)].bg, Color::Indexed(167), "T");
        assert_eq!(buffer[(35, 2)].bg, Color::Reset, "a gap has no background");
        view_mut(&mut app).colouring = Colouring::Plain;
        let terminal = render(&app, 70, 9);
        assert_eq!(terminal.backend().buffer()[(27, 2)].bg, Color::Reset);
    }

    #[test]
    fn the_terminal_cursor_sits_on_the_cursor_cell() {
        let mut app = with_file("tests/data/grid.fa");
        view_mut(&mut app).cursor = Cell { row: 2, col: 4 };
        let mut terminal = render(&app, 70, 9);
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(31, 4)
        );
    }

    #[test]
    fn odd_bytes_take_one_cell_each_and_names_are_cut_by_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.fa");
        // A name of double-width characters, a tab in a name, a non-ASCII
        // byte in a sequence.
        std::fs::write(
            &path,
            b">\xe9\x85\x8d\xe5\x88\x97\xe9\x85\x8d\xe5\x88\x97\nAC\xffT\n>a\tb\nACGT\n",
        )
        .unwrap();
        let mut app = with_file(path.to_str().unwrap());
        app.status.clear(); // a temp path would wrap the status line
        view_mut(&mut app).name_width = 4;
        let terminal = render(&app, 40, 6);
        let l = grid_layout(regions(&app, Rect::new(0, 0, 40, 6)).body, 2, 4, 0);
        let buffer = terminal.backend().buffer();
        let cells = |row: u16| -> String {
            (0..4)
                .map(|i| buffer[(l.cells.x + i, l.cells.y + row)].symbol())
                .collect()
        };
        // Both rows start their residues in the same screen column.
        assert_eq!(
            (cells(0), cells(1)),
            ("AC?T".to_string(), "ACGT".to_string())
        );
        assert_eq!(buffer[(l.names.x, l.names.y)].symbol(), "配");
        assert!(
            (0..l.names.width).any(|i| buffer[(l.names.x + i, l.names.y)].symbol() == "…"),
            "a name too wide for its column is cut with an ellipsis"
        );
        assert!(
            rows(&terminal)[3].starts_with("2 a b "),
            "a tab in a name is drawn as a space"
        );
    }

    #[test]
    fn short_rows_end_where_they_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ragged.fa");
        std::fs::write(&path, ">a\nAC\n>b\nACGT\n").unwrap();
        let mut app = with_file(path.to_str().unwrap());
        app.status.clear();
        view_mut(&mut app).cursor = Cell { row: 0, col: 3 };
        let screen = rows(&render(&app, 40, 6));
        assert!(
            screen[2].ends_with(" AC") && screen[3].ends_with(" ACGT"),
            "{screen:#?}"
        );
        assert_eq!(screen[4], "row 1/2 · col 4/4");
    }

    #[test]
    fn the_information_row_reports_running_work_and_unwritten_edits() {
        let mut app = with_file("tests/data/grid.fa");
        let edit = Command::parse_line("msa delete-cols --cols 1").unwrap();
        app.session.execute(edit).unwrap();
        assert_eq!(info_text(&app).1, "unsaved");
        // A job on a tab that is not an alignment still gets its row.
        let mut app = App::new(Session::new(None));
        app.enqueue(Command::Open {
            path: "tests/data/grid.fa".into(),
            kind: None,
        });
        app.pump();
        assert_eq!(
            info_text(&app),
            (String::new(), "opening grid.fa".to_string())
        );
        let screen = rows(&render(&app, 60, 8));
        assert!(screen[6].ends_with("opening grid.fa"), "{screen:#?}");
    }

    #[test]
    fn the_selection_is_named_and_highlighted() {
        use crate::msa_view::{SelKind, Select};
        let mut app = with_file("tests/data/grid.fa");
        let view = view_mut(&mut app);
        view.cursor = Cell { row: 3, col: 29 };
        view.select = Some(Select {
            kind: SelKind::Block,
            anchor: Cell { row: 1, col: 17 },
        });
        let terminal = render(&app, 70, 9);
        assert_eq!(
            rows(&terminal)[7],
            "row 4/5 · col 30/60 · A · rows 2-4 × cols 18-30"
        );
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(27 + 17, 3)].bg, Color::Indexed(240));
        assert_ne!(buffer[(27 + 16, 3)].bg, Color::Indexed(240));
        assert_ne!(buffer[(27 + 17, 2)].bg, Color::Indexed(240));
        assert_ne!(
            buffer[(3, 3)].bg,
            Color::Indexed(240),
            "names only for whole rows"
        );

        view_mut(&mut app).select = Some(Select {
            kind: SelKind::Rows,
            anchor: Cell { row: 3, col: 0 },
        });
        let terminal = render(&app, 70, 9);
        assert_eq!(rows(&terminal)[7], "row 4/5 · col 30/60 · A · rows 4");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(3, 5)].bg, Color::Indexed(240), "the name too");
        assert_eq!(buffer[(40, 5)].bg, Color::Indexed(240));

        view_mut(&mut app).select = Some(Select {
            kind: SelKind::Cols,
            anchor: Cell { row: 0, col: 27 },
        });
        let terminal = render(&app, 70, 9);
        assert_eq!(rows(&terminal)[7], "row 4/5 · col 30/60 · A · cols 28-30");
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer[(27 + 28, 1)].bg,
            Color::Indexed(240),
            "the ruler too"
        );
        assert_ne!(buffer[(27 + 30, 1)].bg, Color::Indexed(240));

        let view = view_mut(&mut app);
        view.select = None;
        view.picked = Some(crate::msa_view::Picked {
            axis: crate::msa_view::Axis::Rows,
            ranges: "1,4".parse().unwrap(),
        });
        let terminal = render(&app, 70, 9);
        assert_eq!(rows(&terminal)[7], "row 4/5 · col 30/60 · A · rows 1,4 (2)");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(3, 2)].bg, Color::Indexed(240), "row 1's name");
        assert_eq!(buffer[(3, 5)].bg, Color::Indexed(240), "row 4's name");
        assert_ne!(buffer[(3, 3)].bg, Color::Indexed(240), "not row 2's");
    }

    #[test]
    fn a_question_is_shown_on_the_bottom_line() {
        use crate::app::Confirm;
        let mut app = with_file("tests/data/grid.fa");
        let edit = Command::parse_line("msa delete-cols --cols 1").unwrap();
        app.session.execute(edit).unwrap();
        app.mode = Mode::Confirm(Confirm::Quit);
        let screen = rows(&render(&app, 70, 9));
        assert_eq!(
            screen[8],
            "grid.fa has edits not written to a file. Quit anyway? (y/n)"
        );
        assert!(screen[7].ends_with("unsaved"), "{}", screen[7]);
        app.mode = Mode::Confirm(Confirm::Close);
        let screen = rows(&render(&app, 70, 9));
        assert_eq!(
            screen[8],
            "grid.fa has edits not written to a file. Close it anyway? (y/n)"
        );
    }

    #[test]
    fn typed_characters_show_before_they_are_logged() {
        use crate::msa_view::{Pending, Typed};
        let mut app = with_file("tests/data/grid.fa");
        app.mode = Mode::Edit(Typing::Replace);
        view_mut(&mut app).typed = Some(Pending::Replace(Typed {
            row: 0,
            start: 2,
            bytes: b"nn".to_vec(),
        }));
        let mut terminal = render(&app, 70, 9);
        let screen = rows(&terminal);
        assert_eq!(&screen[2][27..33], "ACnnTG");
        assert!(screen[7].ends_with("REPLACE"), "{}", screen[7]);
        let buffer = terminal.backend().buffer();
        assert!(buffer[(29, 2)].modifier.contains(Modifier::UNDERLINED));
        assert!(!buffer[(28, 2)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(27, 2),
            "the cursor is shown while typing"
        );
    }

    /// An app showing `text` on an 80 x 12 screen, with every count done.
    fn with_text(text: &str) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.fa");
        std::fs::write(&path, text).unwrap();
        let mut app = with_file(path.to_str().unwrap());
        app.resize(ratatui::layout::Size::new(80, 12));
        (app, dir)
    }

    fn keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
    }

    /// 4 sequences x 201 columns.
    fn wide() -> String {
        let row = |unit: &str| unit.repeat(50) + "A";
        format!(
            ">a\n{}\n>b\n{}\n>c\n{}\n>d\n{}\n",
            row("ACGT"),
            row("ACGT"),
            row("AC--"),
            row("TTTT")
        )
    }

    /// The overview's layout on an 80 x 12 screen.
    fn overview_layout(app: &App) -> GridLayout {
        let (alignment, view) = app.alignment().unwrap();
        let body = regions(app, Rect::new(0, 0, 80, 12)).body;
        grid_layout(body, alignment.rows(), view.name_width, grid::SCORE_WIDTH)
    }

    #[test]
    fn the_overview_shows_scores_and_colours_blocks_by_matches() {
        let (mut app, _dir) = with_text(&wide());
        keys(&mut app, "-");
        let terminal = render(&app, 80, 12);
        let screen = rows(&terminal);
        let buffer = terminal.backend().buffer();
        let layout = overview_layout(&app);
        let bg = |row: u16, cell: u16| buffer[(layout.cells.x + cell, layout.cells.y + row)].bg;
        let ramp = |step: usize| Some(Color::Indexed(palette::RAMP[step]));
        let line = |row: u16| screen[(layout.cells.y + row) as usize].clone();
        // a: every residue matches.
        assert!(line(0).contains("100%"), "{}", line(0));
        assert_eq!(Some(bg(0, 0)), ramp(4));
        // d: TTTT against A C G T matches one in four, and the last A.
        assert!(line(3).contains(" 25%"), "{}", line(3));
        assert_eq!(Some(bg(3, 0)), ramp(0), "T T against A C");
        assert_eq!(Some(bg(3, 1)), ramp(2), "T T against G T: one of two");
        let info = &screen[regions(&app, Rect::new(0, 0, 80, 12)).info.y as usize];
        assert!(info.starts_with("rows 1/4 · cols 1-2/201"), "{info}");
        assert!(
            info.ends_with("each cell: 2 columns × 1 sequence"),
            "{info}"
        );
        // c, at 4 columns per cell: AC-- is half gaps, so not blank.
        keys(&mut app, "-");
        let terminal = render(&app, 80, 12);
        let buffer = terminal.backend().buffer();
        let layout = overview_layout(&app);
        let screen = rows(&terminal);
        assert!(
            screen[(layout.cells.y + 2) as usize].contains("100%"),
            "{screen:#?}"
        );
        assert_eq!(
            Some(buffer[(layout.cells.x, layout.cells.y + 2)].bg),
            ramp(4)
        );
    }

    /// 40 sequences x 20 columns; the eighth is all T.
    fn tall() -> String {
        (0..40)
            .map(|i| {
                let row = if i == 7 {
                    "TTTTTTTTTTTTTTTTTTTT"
                } else {
                    "ACGTACGTACGTACGTACGT"
                };
                format!(">n{i}\n{row}\n")
            })
            .collect()
    }

    #[test]
    fn a_shrunk_line_shows_the_lowest_score_of_its_sequences() {
        let (mut app, _dir) = with_text(&tall());
        keys(&mut app, "z");
        settle(&mut app);
        let screen = rows(&render(&app, 80, 12));
        let layout = overview_layout(&app);
        let y = layout.cells.y as usize;
        assert!(screen[y].contains(" 25%"), "{}", screen[y]);
        assert!(screen[y + 1].contains("100%"), "{}", screen[y + 1]);
    }

    #[test]
    fn lines_holding_several_sequences_say_how_many() {
        let (mut app, _dir) = with_text(&tall());
        keys(&mut app, "z"); // no pump: no summary is asked for
        let screen = rows(&render(&app, 80, 12));
        let layout = overview_layout(&app);
        let first = &screen[layout.cells.y as usize];
        assert!(first.contains("n0 +7"), "{first}");
        assert!(first.contains('…'), "{first}");
    }

    #[test]
    fn the_overview_survives_tiny_terminals() {
        let (mut app, _dir) = with_text(&wide());
        keys(&mut app, "-");
        for (width, height) in [(5, 3), (1, 1), (12, 4)] {
            render(&app, width, height);
        }
    }

    #[test]
    fn an_insert_past_the_last_column_shows_the_columns_it_adds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trio.fa");
        std::fs::write(
            &path,
            ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n",
        )
        .unwrap();
        let mut app = with_file(path.to_str().unwrap());
        for c in "iXYZ".chars() {
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        let screen = rows(&render(&app, 80, 12));
        let line = |name: &str| screen.iter().find(|line| line.contains(name)).unwrap();
        assert!(line("s1").contains("XYZACGTACGTA"), "{}", line("s1"));
        assert!(line("s2").contains("ACGTTACG-A--"), "{}", line("s2"));
    }

    #[test]
    fn an_insert_shows_the_shifted_row_and_says_insert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trio.fa");
        std::fs::write(
            &path,
            ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n",
        )
        .unwrap();
        let mut app = with_file(path.to_str().unwrap());
        for c in "jjliNN".chars() {
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        let terminal = render(&app, 80, 12);
        let screen = rows(&terminal);
        let line = screen
            .iter()
            .find(|line| line.contains("s3 third one"))
            .unwrap();
        assert!(line.contains("ANNCTACGTA"), "{line}");
        assert!(
            screen.iter().any(|line| line.ends_with("INSERT")),
            "{screen:#?}"
        );
    }

    /// Run a command as the interface would, and everything it sets off.
    fn run(app: &mut App, line: &str) {
        let ran = app.run_command(Command::parse_line(line).unwrap());
        ran.unwrap_or_else(|error| panic!("{line}: {error}"));
        settle(app);
    }

    /// Whether the cell at a row and column of the grid has the background
    /// of a conserved residue.
    fn shaded(app: &App, terminal: &Terminal<TestBackend>, row: u16, col: u16) -> bool {
        let buffer = terminal.backend().buffer();
        let (alignment, view) = app.alignment().unwrap();
        let body = regions(app, buffer.area).body;
        let cells = grid_layout(body, alignment.rows(), view.name_width, 0).cells;
        buffer[(cells.x + col, cells.y + row)].bg == Color::Indexed(24)
    }

    #[test]
    fn conserved_cells_are_shaded_by_default() {
        let app = with_file("tests/data/cons.fa");
        let terminal = render(&app, 60, 9);
        // ACGT-A / ACGA-C / ACTT-G / AGGTTT
        let picture: Vec<String> = (0..4)
            .map(|row| {
                (0..6)
                    .map(|col| {
                        if shaded(&app, &terminal, row, col) {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect()
            })
            .collect();
        assert_eq!(picture, ["####..", "###...", "##.#..", "#.##.."]);
    }

    #[test]
    fn the_shading_follows_edits_and_the_colour_key() {
        let mut app = with_file("tests/data/cons.fa");
        run(&mut app, "msa fill --rows 1-2 --cols 6 --with G"); // column 6: G G G T
        let terminal = render(&app, 60, 9);
        assert!(shaded(&app, &terminal, 0, 5) && !shaded(&app, &terminal, 3, 5));
        run(&mut app, "msa delete-cols --cols 1");
        let terminal = render(&app, 60, 9);
        assert!(
            shaded(&app, &terminal, 0, 0) && !shaded(&app, &terminal, 3, 0),
            "what was column 2 is now column 1"
        );
        view_mut(&mut app).cycle_colouring();
        let terminal = render(&app, 60, 9);
        assert!(!shaded(&app, &terminal, 0, 0), "by residue: no shading");
    }

    #[test]
    fn unaligned_files_fall_back_to_residue_colours() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ragged.fa");
        std::fs::write(&path, ">a\nAC\n>b\nACGT\n").unwrap();
        let mut app = with_file(path.to_str().unwrap());
        app.status.clear();
        let terminal = render(&app, 40, 6);
        let body = regions(&app, Rect::new(0, 0, 40, 6)).body;
        let cells = grid_layout(body, 2, 24, 0).cells;
        assert_eq!(
            terminal.backend().buffer()[(cells.x, cells.y)].bg,
            Color::Indexed(71)
        );
        assert!(app.outbox.is_empty() && app.conserved.is_empty());
    }

    #[test]
    fn help_names_every_key_and_command() {
        let mut app = with_file("tests/data/grid.fa");
        app.mode = Mode::Help;
        let screen = rows(&render(&app, 80, 24)).join("\n");
        for needle in [
            "w save the whole alignment",
            "e extract",
            "Tab completes a file name after :tree link",
            "hjkl",
            "PgUp",
            "Home",
            "right-click",
            "v V C",
            "Esc",
            "undo",
            "redo",
            "overwrite",
            "insert",
            "remove",
            "R type over",
            "z view whole alignment",
            "delete-rows",
            "delete-cols",
            "extract",
            "rename",
            "close",
            ":rescue FILE",
            "Option on macOS",
            "set -g mouse on",
        ] {
            assert!(
                screen.contains(needle),
                "help does not mention {needle:?}\n{screen}"
            );
        }
        for (width, height) in [(30, 8), (10, 3), (1, 1)] {
            render(&app, width, height);
        }
    }

    #[test]
    fn a_long_prompt_line_shows_the_part_around_the_cursor() {
        assert_eq!(window("abcdef", 6, 4), ("def", 3));
        assert_eq!(window("abcdef", 2, 4), ("abcd", 2));
        assert_eq!(window("abcdef", 6, 10), ("abcdef", 6));
        assert_eq!(window("ab漢字", 8, 4), ("字", 2));
        assert_eq!(window("abc", 3, 0), ("", 0));
    }

    #[test]
    fn a_pasted_line_as_long_as_an_alignment_draws_at_once() {
        let text = format!(
            ":msa overwrite --row 1 --col 1 --seq {}",
            "A".repeat(100_000)
        );
        let started = std::time::Instant::now();
        let (visible, column) = window(&text, text.len(), 80);
        assert_eq!((visible.len(), column), (79, 79));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn every_help_page_fits_a_24_row_terminal() {
        // Its own lines, the shared ones, the log line and the border.
        for own in [HELP_ALIGNMENT, HELP_TREE, HELP_BAM] {
            let rows = own.len() + HELP_SHARED.len() + 1 + 2;
            assert!(rows <= 24, "{rows} rows, starting {:?}", own[0]);
        }
    }

    #[test]
    fn every_help_line_fits_a_60_column_terminal() {
        for line in HELP_ALIGNMENT
            .iter()
            .chain(HELP_TREE)
            .chain(HELP_BAM)
            .chain(HELP_SHARED)
        {
            assert!(line.width() <= 56, "{} columns: {line:?}", line.width());
        }
    }

    #[test]
    fn a_long_log_path_shows_its_end_in_help() {
        use seqpeeq_core::SessionLog;
        let dir = tempfile::tempdir().unwrap();
        let log = dir
            .path()
            .join("a-very-long-directory-name".repeat(6))
            .join("session-END.log");
        let mut app = App::new(Session::new(Some(SessionLog::new(log))));
        app.mode = Mode::Help;
        let screen = rows(&render(&app, 60, 24)).join("\n");
        assert!(screen.contains("log: …"), "{screen}");
        assert!(screen.contains("session-END.log"), "{screen}");
    }

    #[test]
    fn the_tab_bar_scrolls_to_keep_the_active_tab_in_view() {
        // Each tab is 10 columns wide, with one column between tabs.
        let titles: Vec<String> = (1..=6).map(|n| format!("file{n}.fa")).collect();
        assert_eq!(tab_ranges(&titles, 80, 0)[..2], [0..10, 11..21]);
        let scrolled = tab_ranges(&titles, 25, 4);
        assert!(scrolled[0].is_empty() && scrolled[1].is_empty() && scrolled[2].is_empty());
        assert_eq!(scrolled[3..], [0..10, 11..21, 22..25]);
        // A tab wider than the screen is cut, not hidden.
        assert_eq!(
            tab_ranges(&["x".repeat(50)], 20, 0),
            std::slice::from_ref(&(0..20))
        );
        assert!(
            tab_ranges(&titles, 0, 2)
                .iter()
                .all(|range| range.is_empty())
        );
    }

    #[test]
    fn a_scrolled_tab_bar_draws_what_its_ranges_say() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Session::new(None));
        for n in 1..=6 {
            let path = dir.path().join(format!("file{n}.fa"));
            std::fs::write(&path, ">a\nACGT\n").unwrap();
            app.enqueue(Command::Open { path, kind: None });
        }
        settle(&mut app);
        app.active = 4;
        let screen = rows(&render(&app, 25, 6));
        assert_eq!(screen[0], " file4.fa │ file5.fa │ fi");
    }

    #[test]
    fn matching_names_are_underlined() {
        use crate::msa_view::Search;
        let mut app = with_file("tests/data/grid.fa");
        view_mut(&mut app).search = Some(Search {
            pattern: "pan".into(),
            matches: vec![1],
            revision: 0,
            names: None,
        });
        let terminal = render(&app, 70, 9);
        let buffer = terminal.backend().buffer();
        assert!(buffer[(2, 3)].modifier.contains(Modifier::UNDERLINED));
        assert!(!buffer[(2, 2)].modifier.contains(Modifier::UNDERLINED));
        // Matches found in an earlier state are not marked on this one.
        view_mut(&mut app).search.as_mut().unwrap().revision = 99;
        let terminal = render(&app, 70, 9);
        assert!(
            !terminal.backend().buffer()[(2, 3)]
                .modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn help_has_a_bam_section() {
        let help = HELP_BAM.join("\n");
        for text in [
            "] [ covered",
            "} { no coverage",
            "m mate",
            "s other split part",
            "' back",
            "g go to, Tab a contig",
            "c list contigs",
            "F toggle filtered",
            "S toggle clips",
            "click, drag, Space select   Esc clear",
            "Tab next BAM in a stack",
            "v v  select a region",
            "e export reads",
            "E export reference",
        ] {
            assert!(help.contains(text), "{text}");
        }
    }

    #[test]
    fn help_has_a_read_axis_section() {
        let help = HELP_BAM.join("\n");
        for text in [
            "a visualize all alignments for a read",
            "A list all alignments for a read",
            "S search the BAM",
        ] {
            assert!(help.contains(text), "{text}");
        }
    }

    #[test]
    fn help_has_a_tree_section() {
        let help = HELP_TREE.join("\n");
        for text in [
            "p link to an open MSA",
            "w save the whole tree   e extract the clade",
            "Enter open/close",
            "b lengths",
            "o re-root on branch",
            "a select the tips' sequences in the MSA",
            "Space, right-click  pick several clades",
            "O re-root at the midpoint",
            "many: tree rename --regex RE --replace TEXT",
        ] {
            assert!(help.contains(text), "{text}");
        }
    }

    #[test]
    fn help_alignment_wording() {
        let help = HELP_ALIGNMENT.join("\n");
        for text in [
            "Shift+arrows jumps",
            "z view whole alignment",
            "c cycle colors",
            "[ ] adjust name width",
            "many: msa rename --regex RE --replace TEXT",
            "Space, right-click a name or number  pick",
        ] {
            assert!(help.contains(text), "{text}");
        }
    }

    fn with_tree() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let nwk = dir.path().join("t.nwk");
        std::fs::write(&nwk, "((a,b),c);").unwrap();
        (with_file(nwk.to_str().unwrap()), dir)
    }

    fn help_screen(app: &mut App, width: u16, height: u16) -> String {
        app.mode = Mode::Help;
        rows(&render(app, width, height)).join("\n")
    }

    #[test]
    fn help_is_for_the_tab_in_view() {
        let (mut bam, _dir) = crate::app::bam::tests::with_bam();
        let screen = help_screen(&mut bam, 80, 24);
        assert!(screen.contains("help: BAM"), "{screen}");
        for has in ["m mate", "q quit"] {
            assert!(screen.contains(has), "{has}\n{screen}");
        }
        for lacks in ["msa delete-rows", "b lengths"] {
            assert!(!screen.contains(lacks), "{lacks}\n{screen}");
        }
        let mut aln = with_file("tests/data/grid.fa");
        let screen = help_screen(&mut aln, 80, 24);
        assert!(screen.contains("help: alignment"), "{screen}");
        assert!(screen.contains("msa delete-rows"));
        assert!(!screen.contains("m mate"));
        let (mut tree, _tdir) = with_tree();
        let screen = help_screen(&mut tree, 80, 24);
        assert!(screen.contains("help: tree"), "{screen}");
        assert!(screen.contains("o re-root"));
        assert!(!screen.contains("m mate"));
        let mut none = App::new(Session::new(None));
        let screen = help_screen(&mut none, 80, 24);
        assert!(screen.contains("┌help─"), "{screen}");
        assert!(screen.contains(":open close use undo redo"), "{screen}");
        for lacks in ["m mate", "msa delete-rows", "o re-root"] {
            assert!(!screen.contains(lacks), "{lacks}\n{screen}");
        }
    }

    #[test]
    fn each_help_page_fits_80_by_24() {
        let (mut bam, _dir) = crate::app::bam::tests::with_bam();
        let (mut tree, _tdir) = with_tree();
        let mut aln = with_file("tests/data/grid.fa");
        for app in [&mut bam, &mut tree, &mut aln] {
            let screen = help_screen(app, 80, 24);
            assert!(screen.contains("in tmux: set -g mouse on"), "{screen}");
            assert!(screen.contains('└') && screen.contains('┘'), "{screen}");
        }
    }

    #[test]
    fn help_on_a_tiny_terminal_draws() {
        let (mut bam, _dir) = crate::app::bam::tests::with_bam();
        let screen = help_screen(&mut bam, 30, 8);
        assert!(screen.contains("help: B"), "{screen}");
    }

    /// A balanced tree of `2^depth` tips named t0, t1, ….
    fn balanced(depth: u32) -> String {
        fn clade(depth: u32, next: &mut u32) -> String {
            if depth == 0 {
                *next += 1;
                return format!("t{}", *next - 1);
            }
            format!("({},{})", clade(depth - 1, next), clade(depth - 1, next))
        }
        format!("{};", clade(depth, &mut 0))
    }

    #[test]
    fn pruning_again_and_again_on_a_short_screen_does_not_crash() {
        // Issue 6: a collapsed row of a node a prune had removed was laid
        // out before the view followed the edit.
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nwk");
        std::fs::write(&path, balanced(6)).unwrap();
        let mut app = with_file(path.to_str().unwrap());
        app.resize(ratatui::layout::Size::new(80, 12));
        settle(&mut app);
        let key = |app: &mut App, c: char| {
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        };
        for _ in 0..20 {
            key(&mut app, 'j');
            key(&mut app, 'd');
            settle(&mut app);
            render(&app, 80, 12);
        }
        let tips = app.tree().unwrap().1.tree().tips().len();
        assert!(tips < 64, "the prunes ran: {tips} tips left");
    }

    fn readme() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
            .expect("README.md at the repo root")
    }

    /// `docs/<name>` at the repo root.
    fn doc(name: &str) -> String {
        let path = format!("{}/../../docs/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("docs/{name}: {e}"))
    }

    /// Every leaf command the parser knows, as its words: `msa delete-cols`.
    pub(crate) fn command_paths() -> Vec<(String, clap::Command)> {
        fn walk(prefix: &str, cmd: &clap::Command, out: &mut Vec<(String, clap::Command)>) {
            for sub in cmd.get_subcommands() {
                let path = format!("{prefix}{}", sub.get_name()).trim().to_string();
                if sub.has_subcommands() {
                    walk(&format!("{path} "), sub, out);
                } else {
                    out.push((path, sub.clone()));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            "",
            &<seqpeeq_core::Command as clap::CommandFactory>::command(),
            &mut out,
        );
        out
    }

    #[test]
    fn keys_doc_has_every_help_line() {
        let keys = doc("keys.md");
        for line in HELP_ALIGNMENT
            .iter()
            .chain(HELP_TREE)
            .chain(HELP_BAM)
            .chain(HELP_SHARED)
        {
            let line = line.trim();
            assert!(!line.is_empty());
            assert!(
                keys.contains(line),
                "docs/keys.md lacks the help line {line:?}"
            );
        }
    }

    #[test]
    fn readme_downloads_this_version() {
        let line = format!("v=v{}", env!("CARGO_PKG_VERSION"));
        assert!(
            readme().contains(&format!("\n{line}\n")),
            "the README's download lines should set {line}"
        );
    }

    #[test]
    fn commands_doc_names_every_command() {
        let commands = doc("commands.md");
        let paths = command_paths();
        assert!(paths.len() >= 20, "only {} commands found", paths.len());
        for (path, _) in &paths {
            assert!(
                commands.contains(&format!("`{path}")),
                "docs/commands.md does not name `{path}`"
            );
        }
    }

    #[test]
    #[ignore = "prints the commands for writing docs/commands.md"]
    fn print_commands() {
        for (path, mut cmd) in command_paths() {
            let usage = cmd.render_usage().to_string();
            let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();
            println!("{path}\t{usage}\t{about}");
            for arg in cmd.get_arguments() {
                let name = arg
                    .get_long()
                    .map(|l| format!("--{l}"))
                    .unwrap_or_else(|| arg.get_id().to_string().to_uppercase());
                let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
                println!("    {name}\t{help}");
            }
        }
    }
}
