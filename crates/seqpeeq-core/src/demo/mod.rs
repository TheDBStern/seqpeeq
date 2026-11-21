//! The example files in `examples/`: a small alignment, its tree, a
//! reference and two BAMs, each planting features to try. Everything comes
//! from fixed seeds, so the same code writes the same files.

pub mod msa;

/// A 64-bit linear congruential generator (Knuth's MMIX constants).
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }

    pub fn base(&mut self) -> u8 {
        b"ACGT"[(self.next_u64() >> 7) as usize % 4]
    }
}
