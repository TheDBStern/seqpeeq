//! The colours. All are indexes into the 256-colour palette, which works
//! over SSH and inside tmux where 24-bit colour often does not.

use ratatui::style::{Color, Modifier, Style};
use seqpeeq_core::Alphabet;

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

#[cfg(test)]
mod tests {
    use super::*;

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
