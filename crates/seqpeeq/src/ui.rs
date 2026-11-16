//! Drawing. Reads the [`App`](crate::app::App) state and never changes it.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};
use seqpeeq_core::Alphabet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Mode};

/// Rows the log pane takes when shown, borders included.
const LOG_HEIGHT: u16 = 6;

/// The most rows a long status message may take at the bottom.
const MAX_STATUS_ROWS: u16 = 4;

/// Draw the whole interface for the current state.
///
/// Rows are handed out by priority, so a small terminal drops pieces instead
/// of overlapping them: bottom line first, then the tab bar, then the body,
/// and the log pane only when the body keeps at least one row. A status
/// message too long for one row wraps onto up to [`MAX_STATUS_ROWS`], because
/// the end of an error message is where the remedy is.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let status = match app.mode {
        Mode::Command(_) => Vec::new(),
        _ => wrap_columns(&app.status, area.width as usize),
    };
    let bottom_height = (status.len() as u16)
        .clamp(1, MAX_STATUS_ROWS)
        .min(area.height.saturating_sub(2).max(1));
    let bottom = Rect {
        y: area.bottom() - bottom_height,
        height: bottom_height,
        ..area
    };
    draw_bottom(frame, app, &status, bottom);

    if area.height > bottom_height {
        draw_tabs(frame, app, Rect { height: 1, ..area });
    }
    if area.height > bottom_height + 1 {
        let middle = Rect {
            y: area.y + 1,
            height: area.height - 1 - bottom_height,
            ..area
        };
        let log_height = if app.show_log && middle.height > LOG_HEIGHT {
            LOG_HEIGHT
        } else {
            0
        };
        let body = Rect {
            height: middle.height - log_height,
            ..middle
        };
        draw_body(frame, app, body);
        if log_height > 0 {
            let log = Rect {
                y: body.bottom(),
                height: log_height,
                ..middle
            };
            draw_log(frame, app, log);
        }
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

/// The longest end of `text` that fits in `width` display columns.
fn tail_columns(text: &str, width: usize) -> &str {
    let mut used = 0;
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        used += c.width().unwrap_or(0);
        if used > width {
            break;
        }
        start = index;
    }
    &text[start..]
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
    let lines = match app.session.datasets().get(app.active) {
        None => vec![Line::from(
            "No files open. :open <path> to open one, ? for help",
        )],
        Some(dataset) => {
            let path = Line::from(dataset.path.display().to_string());
            match dataset.msa() {
                Some(alignment) => vec![
                    path,
                    Line::from(alignment.shape()),
                    Line::styled(
                        match alignment.alphabet() {
                            Alphabet::Nucleotide => "nucleotide",
                            Alphabet::Protein => "protein",
                        },
                        dim(),
                    ),
                ],
                None => vec![
                    path,
                    Line::from(format!("{}, {} bytes", dataset.kind.as_str(), dataset.size)),
                    Line::styled("viewer not built yet", dim()),
                ],
            }
        }
    };
    let block = Block::new().padding(Padding::new(1, 1, 1, 0));
    frame.render_widget(Paragraph::new(lines).block(block), area);
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

fn draw_bottom(frame: &mut Frame, app: &App, status: &[String], area: Rect) {
    if let Mode::Command(buffer) = &app.mode {
        // Show the end of a long line, where the typing happens, and keep one
        // column free for the cursor.
        let text = format!(":{buffer}");
        let visible = tail_columns(&text, area.width as usize - 1);
        let cursor = area.x + visible.width() as u16;
        frame.render_widget(Paragraph::new(visible), area);
        frame.set_cursor_position(Position::new(cursor, area.y));
        return;
    }
    let shown = status.len().min(area.height as usize);
    let mut lines: Vec<String> = status[..shown].to_vec();
    if shown < status.len()
        && let Some(last) = lines.last_mut()
    {
        last.pop();
        last.push('…');
    }
    let style = if app.status.starts_with("error:") {
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
    use crate::app::Mode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use seqpeeq_core::{Command, Session};

    fn terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    fn render(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = terminal(width, height);
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
    }

    /// An app with the fixture alignment open. The path is relative (tests run
    /// in the crate directory), so snapshots hold no machine-specific text.
    fn with_dataset() -> App {
        let mut app = App::new(Session::new(None));
        let open = Command::Open {
            path: "tests/data/aln.fa".into(),
            kind: None,
        };
        app.status = app.session.execute(open).unwrap().message;
        app
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
        app.mode = Mode::Command("open x".to_string());
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
        assert_eq!(
            screen[4], " nucleotide",
            "the body keeps its rows above the status"
        );
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
        app.mode = Mode::Command(format!("open {}END", "/long/path".repeat(5)));
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
        for mode in [
            Mode::Help,
            Mode::Command("open a very long command line".to_string()),
        ] {
            app.mode = mode;
            for (width, height) in [(1, 1), (20, 3), (5, 40), (2, 2), (60, 7)] {
                render(&app, width, height);
            }
        }
    }

    #[test]
    fn tab_ranges_count_wide_characters_by_display_width() {
        // Two double-width characters: 4 columns of title plus the two spaces.
        assert_eq!(tab_ranges(&["配列".to_string()])[0], 0..6);
    }

    #[test]
    fn alignment_body_shows_shape_and_alphabet() {
        let mut app = with_dataset();
        let screen = rows(&render(&app, 60, 8));
        let at = screen
            .iter()
            .position(|row| row.contains("2 sequences x 8 columns") && !row.contains("opened"))
            .expect("the body states the shape");
        assert!(screen[at + 1].contains("nucleotide"), "{screen:?}");
        assert!(
            !screen
                .iter()
                .any(|row| row.contains("viewer not built yet"))
        );

        let cut = Command::parse_line("msa delete-cols --cols 1-3").unwrap();
        app.session.execute(cut).unwrap();
        let screen = rows(&render(&app, 60, 8));
        assert!(
            screen
                .iter()
                .any(|row| row.contains("2 sequences x 5 columns")),
            "{screen:?}"
        );
    }
}
