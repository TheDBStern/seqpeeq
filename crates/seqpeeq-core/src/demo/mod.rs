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

#[cfg(test)]
mod tests {
    use super::bam::tests::records;
    use super::*;
    use std::path::{Path, PathBuf};

    fn committed() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
    }

    #[test]
    fn committed_examples_match_the_generator() {
        let dir = tempfile::tempdir().unwrap();
        write_all(dir.path()).unwrap();
        let stale = |name: &str| {
            format!(
                "examples/{name} is stale: run cargo run -p seqpeeq-core --features testing \
                 --example make_examples -- examples"
            )
        };
        for name in ["aln.fa", "tree.nwk", "ref.fa"] {
            let read = |d: &Path| std::fs::read(d.join(name)).unwrap_or_default();
            assert!(read(dir.path()) == read(&committed()), "{}", stale(name));
        }
        for name in ["reads.bam", "reads2.bam"] {
            assert!(committed().join(name).exists(), "{}", stale(name));
            assert!(
                records(&dir.path().join(name)) == records(&committed().join(name)),
                "{}",
                stale(name)
            );
        }
    }

    #[test]
    fn committed_indexes_are_newer_than_their_bams() {
        let modified = |p: PathBuf| std::fs::metadata(&p).and_then(|m| m.modified());
        for name in ["reads.bam", "reads2.bam"] {
            let bam = modified(committed().join(name)).expect(name);
            let bai = modified(committed().join(format!("{name}.bai"))).expect("an index");
            assert!(
                bai >= bam,
                "{name}.bai is older than {name}, so seqpeeq would ignore it"
            );
        }
    }
}
