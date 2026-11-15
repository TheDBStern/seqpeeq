//! Drawing. Reads the [`App`](crate::app::App) state and never changes it.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};

use crate::app::{App, Mode};

/// Rows the log pane takes when shown, borders included.
const LOG_HEIGHT: u16 = 6;

/// Draw the whole interface for the current state.
///
/// Rows are handed out by priority, so a small terminal drops pieces instead
/// of overlapping them: bottom line first, then the tab bar, then the body,
/// and the log pane only when the body keeps at least one row.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let bottom = Rect {
        y: area.bottom() - 1,
        height: 1,
        ..area
    };
    draw_bottom(frame, app, bottom);

    if area.height >= 2 {
        draw_tabs(frame, app, Rect { height: 1, ..area });
    }
    if area.height >= 3 {
        let middle = Rect {
            y: area.y + 1,
            height: area.height - 2,
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
            draw_log(
                frame,
                app,
                Rect {
                    y: body.bottom(),
                    height: log_height,
                    ..middle
                },
            );
        }
    }
    if matches!(app.mode, Mode::Help) {
        draw_help(frame, app, area);
    }
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
        Some(dataset) => vec![
            Line::from(dataset.path.display().to_string()),
            Line::from(format!("{}, {} bytes", dataset.kind.as_str(), dataset.size)),
            Line::styled("viewer not built yet", dim()),
        ],
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

fn draw_bottom(frame: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        Mode::Command(buffer) => {
            let line = Line::from(format!(":{buffer}"));
            // Keep the cursor on screen when the text is wider than the terminal.
            let column = (line.width() as u16).min(area.width - 1);
            frame.render_widget(Paragraph::new(line), area);
            frame.set_cursor_position(Position::new(area.x + column, area.y));
        }
        _ => {
            let style = if app.status.starts_with("error:") {
                Style::new().fg(Color::Red)
            } else {
                Style::new()
            };
            frame.render_widget(Paragraph::new(app.status.as_str()).style(style), area);
        }
    }
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
            let width = Span::raw(title.as_str()).width() as u16 + 2;
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
}
