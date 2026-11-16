//! Drawing. Reads the [`App`](crate::app::App) state and never changes it.

pub mod grid;

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::msa_view::Cell;

use crate::app::{App, Confirm, Mode};
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
        let wanted = app.busy.is_some() || app.alignment().is_some();
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

/// The layout of the grid of the alignment on the visible tab, on a screen
/// of the size the app was last told of.
pub fn grid_of(app: &App) -> Option<GridLayout> {
    let (alignment, view) = app.alignment()?;
    let area = Rect::new(0, 0, app.screen.width, app.screen.height);
    let body = regions(app, area).body;
    Some(grid::grid_layout(body, alignment.rows(), view.name_width))
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
    let columns = |part: &str| part.width();
    // Start as far left as still shows the cursor.
    let mut start = 0;
    while columns(&text[start..cursor]) + 1 > width && start < cursor {
        start += text[start..].chars().next().map_or(1, char::len_utf8);
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
    (&text[start..end], columns(&text[start..cursor]))
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles = tab_titles(app);
    if titles.is_empty() {
        frame.render_widget(Paragraph::new(" seqpeeq").style(dim()), area);
        return;
    }
    let mut spans = Vec::new();
    for (index, title) in titles.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("│", dim()));
        }
        let style = if index == app.active {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        spans.push(Span::styled(format!(" {title} "), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
    if let Some((alignment, view)) = app.alignment() {
        grid::draw_grid(frame, alignment, view, area);
        // The terminal's own cursor marks the cursor cell.
        let layout = grid::grid_layout(area, alignment.rows(), view.name_width);
        if matches!(app.mode, Mode::Normal)
            && let Some(position) = grid::cursor_position(&layout, view)
        {
            frame.set_cursor_position(position);
        }
        return;
    }
    let lines = match app.session.datasets().get(app.active) {
        None => vec![Line::from(
            "No files open. :open <path> to open one, ? for help",
        )],
        Some(dataset) => vec![
            Line::from(dataset.path.display().to_string()),
            Line::from(format!("{}, {} bytes", dataset.kind.as_str(), dataset.size)),
            Line::styled("viewer not built yet", dim()),
        ],
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
    if let Some((alignment, view)) = app.alignment() {
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
        if let Some(region) = view.region() {
            let (rows, cols) = region.flags();
            let selected = match (rows.rows, cols) {
                (Some(rows), Some(cols)) => format!("rows {rows} × cols {cols}"),
                (Some(rows), None) => format!("rows {rows}"),
                (None, Some(cols)) => format!("cols {cols}"),
                (None, None) => String::new(),
            };
            left.push_str(&format!(" · {selected}"));
        }
        if alignment.is_modified() {
            right.push("unsaved".to_string());
        }
    }
    (left, right.join(" · "))
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
fn bottom_text(app: &App) -> String {
    let Mode::Confirm(confirm) = app.mode else {
        return app.status.clone();
    };
    let unwritten = app.unwritten();
    let subject = match (confirm, unwritten.as_slice()) {
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

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = [
        (":", "command line"),
        ("Tab", "next tab"),
        ("L", "toggle log"),
        ("?", "help"),
        ("q", "quit"),
    ]
    .iter()
    .map(|(key, what)| Line::from(format!("{key:<5}{what}")))
    .collect();
    if let Some(path) = app.session.log_path() {
        lines.push(Line::from(format!("log: {}", path.display())));
    }
    let widest = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    // Content plus a border and one column of padding each side.
    let width = (widest + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .title("help")
        .padding(Padding::horizontal(1));
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
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

/// The columns each tab occupies in the tab bar. A tab is drawn as its title
/// with one space either side, and tabs are separated by one `│` column.
/// Drawing and click handling both use this, so they cannot disagree.
pub fn tab_ranges(titles: &[String]) -> Vec<Range<u16>> {
    let mut start = 0u16;
    titles
        .iter()
        .map(|title| {
            let width = title.width() as u16 + 2;
            let range = start..start.saturating_add(width);
            start = range.end.saturating_add(1);
            range
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Done, Mode, Prompt, PromptKind, Request};
    use crate::line::LineEdit;
    use crate::msa_view::{Cell, Colouring};
    use crate::ui::grid::grid_layout;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Position;
    use seqpeeq_core::{Command, Session};

    fn terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    /// The `:` line holding `text`, with the cursor at its end.
    fn command_line(text: &str) -> Mode {
        Mode::Prompt(Prompt {
            kind: PromptKind::Command,
            line: LineEdit::new(text),
        })
    }

    fn render(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = terminal(width, height);
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
    }

    /// Do, on this thread, the work the loop would hand to worker threads.
    fn settle(app: &mut App) {
        loop {
            app.pump();
            let Some(request) = app.outbox.pop() else {
                break;
            };
            match request {
                Request::Run(job) => app.handle_done(Done::Command(job.run())),
            }
        }
    }

    /// An app with one file open.
    fn with_file(path: &str) -> App {
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

    /// The text of each screen row, trailing spaces removed.
    fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
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
        assert_eq!(tab_ranges(&["配列".to_string()])[0], 0..6);
    }

    #[test]
    fn the_cursor_is_drawn_where_the_typing_is() {
        let mut app = with_dataset();
        let mut line = LineEdit::new("open abc");
        for _ in 0..4 {
            line.key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        app.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Command,
            line,
        });
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
        let l = grid_layout(regions(&app, Rect::new(0, 0, 40, 6)).body, 2, 4);
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
}
