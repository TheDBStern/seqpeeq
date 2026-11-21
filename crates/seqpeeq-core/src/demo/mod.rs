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

pub mod bam;

/// The files [`write_all`] writes.
pub const FILES: [&str; 5] = ["aln.fa", "tree.nwk", "ref.fa", "reads.bam", "reads2.bam"];

/// Write every example file into `dir`; the BAMs get no index.
pub fn write_all(dir: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let (fasta, newick) = msa::alignment_and_tree(0x5eed_0a11);
    std::fs::write(dir.join("aln.fa"), fasta)?;
    std::fs::write(dir.join("tree.nwk"), newick)?;
    let reference = bam::reference(0x5eed_0e01);
    let mut out = std::io::BufWriter::new(std::fs::File::create(dir.join("ref.fa"))?);
    for (name, seq) in &reference {
        writeln!(out, ">{name}")?;
        for line in seq.chunks(60) {
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()?;
    bam::write_reads(&dir.join("reads.bam"), &reference, bam::Sample::Main)?;
    bam::write_reads(&dir.join("reads2.bam"), &reference, bam::Sample::Second)
}
