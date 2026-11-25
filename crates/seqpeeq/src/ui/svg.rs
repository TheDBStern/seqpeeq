//! A rendered screen as an SVG picture, for the README's screens.

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// A cell's size in pixels, for 14px text.
const CELL_W: f64 = 8.4;
const CELL_H: f64 = 17.0;
/// The terminal's own colors, for `Color::Reset`.
const BACKGROUND: &str = "#1e1e1e";
const FOREGROUND: &str = "#d4d4d4";
const FONTS: &str = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";

/// xterm's first sixteen colors.
const ANSI: [&str; 16] = [
    "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
    "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
];

/// `color` as `#rrggbb`, or `None` for the terminal's default.
fn hex(color: Color) -> Option<String> {
    let index = match color {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => return Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Indexed(n) => n,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
    };
    Some(match index {
        0..=15 => ANSI[index as usize].to_string(),
        16..=231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let n = (index - 16) as usize;
            let (r, g, b) = (LEVELS[n / 36], LEVELS[n / 6 % 6], LEVELS[n % 6]);
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        _ => {
            let v = 8 + 10 * (index - 232);
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    })
}

/// A number of pixels without float noise: `25.2`, not `25.200000000000003`.
fn px(v: f64) -> String {
    let text = format!("{v:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// How a run of cells looks.
#[derive(Clone, PartialEq)]
struct Look {
    fg: String,
    /// `None` is the terminal's background.
    bg: Option<String>,
    bold: bool,
    dim: bool,
}

/// Cells in a row that look the same, from column `x`.
struct Run {
    x: u16,
    width: u16,
    text: String,
    look: Look,
}

/// The screen in `buffer`, drawn as an SVG of a dark terminal.
pub(crate) fn buffer_to_svg(buffer: &Buffer) -> String {
    let area = buffer.area;
    let (w, h) = (
        px(area.width as f64 * CELL_W),
        px(area.height as f64 * CELL_H),
    );
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\" font-family=\"{FONTS}\" font-size=\"14\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"{BACKGROUND}\"/>\n"
    );
    let mut texts = String::new();
    for y in 0..area.height {
        for run in runs(buffer, area.y + y) {
            let x = px(run.x as f64 * CELL_W);
            if let Some(bg) = &run.look.bg {
                let _ = writeln!(
                    svg,
                    "<rect x=\"{x}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{bg}\"/>",
                    px(y as f64 * CELL_H),
                    px(run.width as f64 * CELL_W),
                    px(CELL_H)
                );
            }
            let text = run.text.trim_end_matches(' ');
            if text.is_empty() {
                continue;
            }
            let columns = run.width - (run.text.len() - text.len()) as u16;
            let _ = write!(
                texts,
                "<text x=\"{x}\" y=\"{}\" textLength=\"{}\" fill=\"{}\"",
                px(y as f64 * CELL_H + 13.0),
                px(columns as f64 * CELL_W),
                run.look.fg
            );
            if run.look.bold {
                texts.push_str(" font-weight=\"bold\"");
            }
            if run.look.dim {
                texts.push_str(" opacity=\"0.5\"");
            }
            let _ = writeln!(texts, " xml:space=\"preserve\">{}</text>", escape(text));
        }
    }
    svg.push_str(&texts);
    svg.push_str("</svg>\n");
    svg
}

/// The runs of row `y`. A wide character is a run of its own, so every
/// run's text is as many columns wide as its cells.
fn runs(buffer: &Buffer, y: u16) -> Vec<Run> {
    let area = buffer.area;
    let mut runs: Vec<Run> = Vec::new();
    let mut x = area.x;
    let mut wide_before = false;
    while x < area.x + area.width {
        let cell = &buffer[(x, y)];
        let symbol = if cell.symbol().is_empty() {
            " "
        } else {
            cell.symbol()
        };
        let width = symbol.width().max(1) as u16;
        let (mut fg, mut bg) = (
            hex(cell.fg).unwrap_or_else(|| FOREGROUND.to_string()),
            hex(cell.bg),
        );
        if cell.modifier.contains(Modifier::REVERSED) {
            let back = bg.unwrap_or_else(|| BACKGROUND.to_string());
            bg = Some(fg);
            fg = back;
        }
        let look = Look {
            fg,
            bg,
            bold: cell.modifier.contains(Modifier::BOLD),
            dim: cell.modifier.contains(Modifier::DIM),
        };
        match runs.last_mut() {
            Some(run) if run.look == look && width == 1 && !wide_before => {
                run.text.push_str(symbol);
                run.width += 1;
            }
            _ => runs.push(Run {
                x: x - area.x,
                width,
                text: symbol.to_string(),
                look,
            }),
        }
        wide_before = width > 1;
        x += width;
    }
    runs
}

/// A small square of `color`, for the legends' color swatches.
pub(crate) fn swatch_svg(color: Color) -> String {
    let fill = hex(color).unwrap_or_else(|| FOREGROUND.to_string());
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"12\" height=\"12\" viewBox=\"0 0 12 12\">\
         <rect width=\"12\" height=\"12\" rx=\"2\" fill=\"{fill}\"/></svg>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier, Style};

    fn buffer(width: u16, height: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, width, height))
    }

    #[test]
    fn a_swatch_is_a_square_of_its_color() {
        let svg = swatch_svg(Color::Indexed(160));
        assert!(
            svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
            "{svg}"
        );
        assert!(svg.contains("width=\"12\" height=\"12\""), "{svg}");
        assert!(svg.contains("fill=\"#d70000\""), "{svg}");
        assert!(svg.ends_with("</svg>\n"), "{svg}");
        assert!(
            swatch_svg(Color::Reset).contains(&format!("fill=\"{FOREGROUND}\"")),
            "the terminal's own color"
        );
    }

    #[test]
    fn a_plain_buffer_is_one_text_per_row() {
        let mut b = buffer(3, 2);
        b.set_string(0, 0, "abc", Style::new());
        b.set_string(0, 1, "def", Style::new());
        let svg = buffer_to_svg(&b);
        assert!(svg.starts_with("<svg"), "{svg}");
        assert_eq!(svg.matches("<text").count(), 2, "{svg}");
        assert!(
            svg.contains(">abc</text>") && svg.contains(">def</text>"),
            "{svg}"
        );
    }

    #[test]
    fn styles_split_runs_and_map_xterm_colors() {
        let mut b = buffer(4, 1);
        b.set_string(0, 0, "ab", Style::new());
        b.set_string(
            2,
            0,
            "cd",
            Style::new().fg(Color::Indexed(71)).bg(Color::Indexed(24)),
        );
        let svg = buffer_to_svg(&b);
        assert_eq!(svg.matches("<text").count(), 2, "{svg}");
        assert!(svg.contains(r##"fill="#5faf5f""##), "{svg}");
        assert!(
            svg.contains(r##"<rect x="16.8" y="0" width="16.8" height="17" fill="#005f87"/>"##),
            "{svg}"
        );
        // Grays and the first sixteen map too.
        b.set_string(0, 0, "ab", Style::new().fg(Color::Indexed(244)));
        assert!(buffer_to_svg(&b).contains(r##"fill="#808080""##));
    }

    #[test]
    fn reversed_swaps_colors() {
        let mut b = buffer(1, 1);
        b.set_string(0, 0, "x", Style::new().add_modifier(Modifier::REVERSED));
        let svg = buffer_to_svg(&b);
        // The default foreground becomes the background, and the reverse.
        assert!(
            svg.contains(r##"width="8.4" height="17" fill="#d4d4d4""##),
            "{svg}"
        );
        assert!(svg.contains(r##"fill="#1e1e1e" xml:space"##), "{svg}");
    }

    #[test]
    fn bold_and_dim_are_attributes() {
        let mut b = buffer(2, 1);
        b.set_string(0, 0, "a", Style::new().add_modifier(Modifier::BOLD));
        b.set_string(1, 0, "b", Style::new().add_modifier(Modifier::DIM));
        let svg = buffer_to_svg(&b);
        assert!(
            svg.contains(r#"font-weight="bold""#) && svg.contains(r#"opacity="0.5""#),
            "{svg}"
        );
    }

    #[test]
    fn markup_is_escaped() {
        let mut b = buffer(3, 1);
        b.set_string(0, 0, "<&>", Style::new());
        let svg = buffer_to_svg(&b);
        assert!(svg.contains("&lt;&amp;&gt;"), "{svg}");
        assert!(!svg.contains("<&"), "{svg}");
    }

    #[test]
    fn wide_characters_keep_columns() {
        let mut b = buffer(4, 1);
        b.set_string(0, 0, "a世b", Style::new());
        let svg = buffer_to_svg(&b);
        assert!(
            svg.contains(r#"x="25.2""#) && svg.contains(">b</text>"),
            "{svg}"
        );
        assert!(svg.contains(">世</text>"), "{svg}");
    }
}
