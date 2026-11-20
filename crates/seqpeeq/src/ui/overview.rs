//! The zoomed-out overview: each cell a block of columns of one or more
//! sequences, coloured by how many of its residues match their column's
//! conserved residue, with each line's match score beside its name.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use seqpeeq_core::{Alignment, Conservation, Summary, Tally};
use unicode_width::UnicodeWidthStr;

use super::grid::{GridLayout, SCORE_WIDTH, fit, grid_layout};
use crate::msa_view::{MsaView, Shape};
use crate::palette;

/// Draw the overview of `alignment` in `body`, at the view's scale. Lines
/// of several sequences are drawn from `summary` when it is of the
/// alignment's shape; without one, from their first sequence alone.
pub fn draw_overview(
    frame: &mut Frame,
    alignment: &Alignment,
    view: &MsaView,
    conserved: Option<&Conservation>,
    summary: Option<&Summary>,
    body: Rect,
) {
    let layout = grid_layout(body, alignment.rows(), view.name_width, SCORE_WIDTH);
    let GridLayout {
        ruler,
        numbers,
        names,
        score,
        cells,
    } = layout;
    let aln = Shape::of(alignment);
    let (per_line, per_cell) = (view.scale.rows, view.scale.cols);
    let conserved = conserved.filter(|found| found.cols() == aln.cols);
    let counted = conserved.is_some();
    let summary =
        summary.filter(|summary| summary.rows() == aln.rows && summary.cols() == aln.cols);
    // Cells are drawn from the summary when they hold whole blocks of it.
    let blocks = summary.filter(|summary| per_cell.is_multiple_of(summary.block()));
    let selected = view.region(aln);
    // A line or cell is shaded when any of what it stands for is selected:
    // rows picked at full size need not start a line.
    let touched = |rows: std::ops::Range<usize>, cols: std::ops::Range<usize>| {
        selected
            .as_ref()
            .is_some_and(|region| region.touches(rows, cols))
    };
    let whole_rows = selected
        .as_ref()
        .is_some_and(|region| region.cols.is_none());
    let whole_cols = selected
        .as_ref()
        .is_some_and(|region| region.rows.is_none());
    let buffer = frame.buffer_mut();
    let dim = Style::new().add_modifier(Modifier::DIM);

    // The real column of the first cell on screen and of every tenth,
    // each left out where it would run into the one before or off the edge.
    let mut free_from = ruler.x;
    for offset in 0..ruler.width {
        let col = view.left + offset as usize * per_cell;
        if col >= aln.cols {
            break;
        }
        let x = ruler.x + offset;
        if offset % 10 == 0 {
            let label = (col + 1).to_string();
            if x >= free_from && x + label.len() as u16 <= ruler.right() {
                buffer.set_string(x, ruler.y, &label, dim);
                free_from = x + label.len() as u16 + 1;
            }
        }
        if whole_cols && touched(0..aln.rows, col..(col + per_cell).min(aln.cols)) {
            buffer[(x, ruler.y)].set_style(palette::SELECTED);
        }
    }

    let mut tallies: Vec<Tally> = Vec::with_capacity(cells.width as usize);
    for line in 0..cells.height {
        let first = view.top + line as usize * per_line;
        if first >= aln.rows {
            break;
        }
        let last = (first + per_line).min(aln.rows);
        let y = cells.y + line;

        if numbers.width > 1 {
            let width = numbers.width as usize - 1;
            let number = format!("{:>width$}", first + 1);
            buffer.set_stringn(numbers.x, y, number, width, dim);
        }
        if names.width > 1 {
            let width = names.width as usize - 1;
            let others = last - first - 1;
            let name = if others == 0 {
                fit(alignment.name(first), width)
            } else {
                let more = format!(" +{others}");
                let room = width.saturating_sub(more.width());
                fit(alignment.name(first), room) + &more
            };
            let found = view.search.as_ref().is_some_and(|search| {
                search.revision == alignment.revision() && search.any_in(first..last)
            });
            let style = if found {
                Style::new().add_modifier(Modifier::UNDERLINED)
            } else {
                Style::new()
            };
            buffer.set_stringn(names.x, y, name, width, style);
        }
        if whole_rows && touched(first..last, 0..aln.cols) {
            let label = Rect {
                y,
                height: 1,
                width: (numbers.width + names.width).saturating_sub(1),
                ..numbers
            };
            buffer.set_style(label, palette::SELECTED);
        }

        // The line's lowest score, from the summary when the line holds
        // several sequences.
        if score.width > 0 {
            let lowest = match (counted, summary) {
                (false, _) => None,
                (true, Some(summary)) => Some(
                    (first..last)
                        .filter_map(|row| summary.row(row).score())
                        .min(),
                ),
                (true, None) if per_line == 1 => {
                    alignment.view().row_tallies(
                        first,
                        0..aln.cols,
                        aln.cols,
                        conserved,
                        &mut tallies,
                    );
                    Some(tallies.first().and_then(Tally::score))
                }
                (true, None) => None,
            };
            let text = match lowest {
                None => "  …  ".to_string(),
                Some(None) => String::new(),
                Some(Some(score)) => format!("{score:>3}% "),
            };
            buffer.set_stringn(score.x, y, text, score.width as usize, Style::new());
        }

        let on_screen = view.left..view.left + cells.width as usize * per_cell;
        if blocks.is_none() {
            alignment
                .view()
                .row_tallies(first, on_screen, per_cell, conserved, &mut tallies);
        }
        for offset in 0..cells.width {
            let col = view.left + offset as usize * per_cell;
            if col >= aln.cols {
                break;
            }
            let tally = match blocks {
                Some(summary) => {
                    let block = summary.block();
                    let from = col / block;
                    summary.tally(first..last, from..from + per_cell / block)
                }
                None => tallies.get(offset as usize).copied().unwrap_or_default(),
            };
            let style = if touched(first..last, col..(col + per_cell).min(aln.cols)) {
                palette::SELECTED
            } else {
                palette::overview(tally, counted)
            };
            buffer[(cells.x + offset, y)].set_char(' ').set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zoom::levels;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use seqpeeq_core::{Progress, summary_block};
    use std::time::Instant;

    /// Run with `SEQPEEQ_BIG=<a 100,000 x 30,000 alignment> cargo test
    /// --release -p seqpeeq -- --ignored a_level_step`.
    #[test]
    #[ignore = "needs a large alignment named by SEQPEEQ_BIG"]
    fn a_level_step_on_the_largest_alignment_draws_quickly() {
        let path = std::env::var("SEQPEEQ_BIG").expect("SEQPEEQ_BIG names the file");
        let started = Instant::now();
        let alignment = Alignment::load(std::path::Path::new(&path)).unwrap();
        eprintln!("opened in {:?}", started.elapsed());
        let started = Instant::now();
        let conserved = alignment.view().conservation(&Progress::new()).unwrap();
        eprintln!("counted in {:?}", started.elapsed());
        let started = Instant::now();
        let summary = alignment
            .view()
            .summary(&conserved, &Progress::new())
            .unwrap();
        eprintln!(
            "summarised in {:?}, {} MB",
            started.elapsed(),
            summary.bytes() >> 20
        );

        let area = Rect::new(0, 0, 120, 40);
        let aln = Shape::of(&alignment);
        let mut view = MsaView::new();
        let cells = grid_layout(area, aln.rows, view.name_width, SCORE_WIDTH).cells;
        let screen = Shape {
            rows: cells.height as usize,
            cols: cells.width as usize,
        };
        let levels = levels(aln, screen, summary_block(aln.rows, aln.cols));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for (zoom, scale) in levels.iter().enumerate().skip(1) {
            view.zoom = zoom;
            view.scale = *scale;
            view.snap();
            let started = Instant::now();
            terminal
                .draw(|frame| {
                    draw_overview(
                        frame,
                        &alignment,
                        &view,
                        Some(&conserved),
                        Some(&summary),
                        area,
                    )
                })
                .unwrap();
            let took = started.elapsed();
            eprintln!("level {zoom} {scale:?}: {took:?}");
            assert!(took.as_millis() < 100, "level {zoom} took {took:?}");
        }
    }
}
