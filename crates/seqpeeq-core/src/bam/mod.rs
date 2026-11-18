//! Mapped reads: sorted BAM files, read by region through their index.

mod region;
#[cfg(test)]
pub(crate) mod testing;

pub use region::{Region, parse_region};

/// A reference sequence named in a BAM's header, with the read counts its
/// index holds (`None` when the index keeps no counts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contig {
    pub name: String,
    pub length: u64,
    pub mapped: Option<u64>,
    pub unmapped: Option<u64>,
}

/// `n` with commas between groups of three digits: `1,204,551`.
pub(crate) fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}
