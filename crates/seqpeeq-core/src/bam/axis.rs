//! Where on a read its alignments lie, for the read-as-axis view.

/// A CIGAR string as operations; `None` for `*`, an empty string, or text
/// that is not a CIGAR.
pub fn parse_cigar(text: &str) -> Option<Vec<(char, u32)>> {
    if text.is_empty() || text == "*" {
        return None;
    }
    let mut ops = Vec::new();
    let mut number: Option<u32> = None;
    for c in text.chars() {
        if let Some(d) = c.to_digit(10) {
            number = Some(number.unwrap_or(0).checked_mul(10)?.checked_add(d)?);
        } else if "MIDNSHP=X".contains(c) {
            ops.push((c, number.take()?));
        } else {
            return None;
        }
    }
    (number.is_none() && !ops.is_empty()).then_some(ops)
}

/// The first and last bases of the read that `cigar` aligns, and the
/// read's whole length (soft and hard clips included), 1-based and in the
/// orientation the read was sequenced: an alignment to the reverse strand
/// is flipped back. `None` when nothing is aligned.
pub fn read_span(cigar: &[(char, u32)], reverse: bool) -> Option<(u64, u64, u64)> {
    let mut at = 0u64;
    let mut first: Option<u64> = None;
    let mut last = 0u64;
    for &(op, len) in cigar {
        let len = u64::from(len);
        match op {
            'M' | '=' | 'X' | 'I' => {
                if len > 0 {
                    first.get_or_insert(at + 1);
                    last = at + len;
                }
                at += len;
            }
            'S' | 'H' => at += len,
            _ => {}
        }
    }
    let first = first?;
    let length = at;
    Some(if reverse {
        (length - last + 1, length - first + 1, length)
    } else {
        (first, last, length)
    })
}

/// Which read of a pair a record is: 1, 2, or 0 for a read that is not one
/// of a pair.
pub fn segment(flags: u16) -> u8 {
    match (flags & 0x40 != 0, flags & 0x80 != 0) {
        (true, false) => 1,
        (false, true) => 2,
        _ => 0,
    }
}
