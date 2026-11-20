//! The colours. All are indexes into the 256-colour palette, which works
//! over SSH and inside tmux where 24-bit colour often does not.

use ratatui::style::{Color, Modifier, Style};
use seqpeeq_core::{Alphabet, Tally};

/// Cells that carry their column's conserved residue.
pub const SHADE: Style = Style::new().bg(Color::Indexed(24)).fg(Color::Indexed(255));

/// A stretch of reference marked with `v` or a drag on the ruler.
pub const STRETCH: Style = Style::new().bg(Color::Indexed(23)).fg(Color::Indexed(255));

/// Selected cells, names and ruler columns.
/// A tip a search matched, or a collapsed clade holding one.
pub const MATCH: Style = Style::new()
    .fg(Color::Indexed(226))
    .add_modifier(Modifier::BOLD);

pub const SELECTED: Style = Style::new().bg(Color::Indexed(240)).fg(Color::Indexed(255));

/// The style of one residue when colouring by residue: black on a colour
/// for the residues that have one, dim for gaps and everything else.
pub fn residue(alphabet: Alphabet, residue: u8) -> Style {
    let residue = residue.to_ascii_uppercase();
    let colour = match alphabet {
        Alphabet::Nucleotide => match residue {
            b'A' => Some(71),
            b'C' => Some(68),
            b'G' => Some(178),
            b'T' | b'U' => Some(167),
            _ => None,
        },
        // Grouped by chemistry, after the scheme Clustal made familiar.
        Alphabet::Protein => match residue {
            b'A' | b'I' | b'L' | b'M' | b'F' | b'W' | b'V' => Some(68),
            b'K' | b'R' => Some(167),
            b'D' | b'E' => Some(133),
            b'N' | b'Q' | b'S' | b'T' => Some(71),
            b'C' => Some(175),
            b'G' => Some(172),
            b'P' => Some(178),
            b'H' | b'Y' => Some(37),
            _ => None,
        },
    };
    match colour {
        Some(index) => Style::new()
            .bg(Color::Indexed(index))
            .fg(Color::Indexed(16)),
        None => Style::new().add_modifier(Modifier::DIM),
    }
}

/// A base with bases inserted just before it.
pub const INSERTION: Style = Style::new().bg(Color::Indexed(93)).fg(Color::Indexed(255));

/// The part of the whole-contig strip that the view shows.
pub const IN_VIEW: Style = Style::new().bg(Color::Indexed(238));

/// Stretches of a read that no placement covers, on its axis.
pub const UNALIGNED: Style = Style::new().bg(Color::Indexed(236));

/// A read's soft-clipped ends, zoomed out.
pub const CLIP: Style = Style::new().fg(Color::Indexed(244));

/// The coverage row's bars.
pub const DEPTH: Style = Style::new().fg(Color::Indexed(68));

/// A mismatching base's letter, coloured as the residue colours are.
pub fn base(base: u8) -> Style {
    match base.to_ascii_uppercase() {
        b'A' => Style::new().fg(Color::Indexed(71)),
        b'C' => Style::new().fg(Color::Indexed(68)),
        b'G' => Style::new().fg(Color::Indexed(178)),
        b'T' | b'U' => Style::new().fg(Color::Indexed(167)),
        _ => Style::new(),
    }
}

/// A read's colour, for what is unusual about it.
pub fn read(kind: crate::bam_view::ReadKind) -> Style {
    use crate::bam_view::ReadKind;
    match kind {
        ReadKind::Split => Style::new().fg(Color::Indexed(170)),
        ReadKind::MateElsewhere => Style::new().fg(Color::Indexed(208)),
        ReadKind::Improper => Style::new().fg(Color::Indexed(75)),
        ReadKind::Plain => Style::new(),
    }
}

/// The overview's blue ramp, from few residues matching their column's
/// conserved residue to all of them.
pub const RAMP: [u8; 5] = [189, 153, 111, 68, 24];

/// An overview cell of residues before the conserved residues are counted.
pub const UNCOUNTED: u8 = 245;

/// How an overview cell holding `tally` is drawn. A cell more than half
/// gaps has no colour; otherwise its background is the ramp's step for the
/// share of residues that match, or grey while nothing is `counted`.
pub fn overview(tally: Tally, counted: bool) -> Style {
    if tally.cells == 0 || tally.is_gappy() {
        return Style::new();
    }
    let colour = if counted {
        let step = (5 * tally.matches / tally.residues).min(4) as usize;
        RAMP[step]
    } else {
        UNCOUNTED
    };
    Style::new().bg(Color::Indexed(colour))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_cells_step_up_the_ramp_with_matches() {
        let cell = |residues, matches| Tally {
            cells: 10,
            residues,
            matches,
        };
        let bg = |tally, counted| overview(tally, counted).bg;
        assert_eq!(bg(cell(10, 10), true), Some(Color::Indexed(RAMP[4])));
        assert_eq!(bg(cell(10, 1), true), Some(Color::Indexed(RAMP[0])));
        assert_eq!(bg(cell(10, 5), true), Some(Color::Indexed(RAMP[2])));
        assert_eq!(bg(cell(5, 5), false), Some(Color::Indexed(UNCOUNTED)));
        assert_eq!(bg(cell(4, 4), true), None, "more than half gaps");
        assert_eq!(bg(Tally::default(), true), None);
    }

    #[test]
    fn case_does_not_change_the_colour_and_gaps_have_none() {
        for alphabet in [Alphabet::Nucleotide, Alphabet::Protein] {
            assert_eq!(residue(alphabet, b'a'), residue(alphabet, b'A'));
            for gap in [b'-', b'.', b'?', b'X', 0xff] {
                assert_eq!(residue(alphabet, gap).bg, None, "{gap}");
            }
        }
        assert_eq!(
            residue(Alphabet::Nucleotide, b'U').bg,
            residue(Alphabet::Nucleotide, b'T').bg
        );
        assert_ne!(
            residue(Alphabet::Protein, b'K').bg,
            residue(Alphabet::Protein, b'D').bg
        );
    }
}
