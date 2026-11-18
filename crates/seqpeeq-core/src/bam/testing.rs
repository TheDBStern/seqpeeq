//! Small BAM files built from SAM text, for tests.

use std::fs::File;
use std::path::{Path, PathBuf};

use noodles::bam;
use noodles::sam;
use noodles::sam::alignment::io::Write as _;

/// Write `sam_text` (a SAM header and records) as `dir/name`, a BAM file
/// with no index.
pub fn bam_from_sam(dir: &Path, name: &str, sam_text: &str) -> PathBuf {
    let mut reader = sam::io::Reader::new(sam_text.as_bytes());
    let header = reader.read_header().expect("a SAM header");
    let path = dir.join(name);
    let mut writer = bam::io::Writer::new(File::create(&path).expect("create the BAM"));
    writer.write_header(&header).expect("write the header");
    for record in reader.record_bufs(&header) {
        let record = record.expect("a SAM record");
        writer
            .write_alignment_record(&header, &record)
            .expect("write a record");
    }
    writer.finish(&header).expect("finish the BAM");
    path
}

/// The files [`fixture`] writes.
pub struct Fixture {
    pub bam: PathBuf,
    pub reference: PathBuf,
}

/// The reference bases of the fixture's contig `c` (`c1`, `c2` or `c3`)
/// from `a` to `b`, 1-based and inclusive.
pub fn ref_bases(c: &str, a: u64, b: u64) -> String {
    let seq = reference_seq(c);
    seq[a as usize - 1..b as usize].to_string()
}

fn reference_seq(c: &str) -> String {
    match c {
        "c1" => "ACGT".repeat(250),
        "c2" => "TTGCA".repeat(100),
        "c3" => "A".repeat(300),
        _ => panic!("no contig {c} in the fixture"),
    }
}

/// The reverse complement of `bases`.
pub fn revcomp(bases: &str) -> String {
    bases
        .bytes()
        .rev()
        .map(|b| match b {
            b'A' => 'T',
            b'C' => 'G',
            b'G' => 'C',
            b'T' => 'A',
            other => other as char,
        })
        .collect()
}

/// The fixture's reads, as the plan's table gives them.
pub fn fixture_sam() -> String {
    let r = ref_bases;
    let mut r1 = r("c1", 101, 150).into_bytes();
    r1[9] = b'G'; // position 110 reads G where the reference has C
    let r1 = String::from_utf8(r1).unwrap();
    let r2 = format!(
        "{}{}{}",
        r("c2", 51, 60),
        r("c1", 121, 150),
        r("c1", 156, 165)
    );
    let rows = [
        format!("r1\t99\tc1\t101\t60\t50M\t=\t301\t250\t{r1}\t*\tMD:Z:9C40"),
        format!("r2\t0\tc1\t121\t60\t10S30M5D10M\t*\t0\t0\t{r2}\t*\tSA:Z:c2,51,+,10M40H,60,0;"),
        format!(
            "r1\t147\tc1\t301\t60\t50M\t=\t101\t-250\t{}\t*",
            r("c1", 301, 350)
        ),
        format!("r3\t0\tc1\t701\t0\t40M\t*\t0\t0\t{}\t*", r("c1", 701, 740)),
        "r4\t256\tc1\t711\t60\t40M\t*\t0\t0\t*\t*".to_string(),
        format!(
            "r5\t1024\tc1\t721\t60\t40M\t*\t0\t0\t{}\t*",
            r("c1", 721, 760)
        ),
        format!(
            "r6\t97\tc1\t901\t60\t50M\tc2\t201\t0\t{}\t*",
            r("c1", 901, 950)
        ),
        format!(
            "r2\t2048\tc2\t51\t60\t10M40H\t*\t0\t0\t{}\t*\tSA:Z:c1,121,+,10S30M5D10M,60,0;",
            r("c2", 51, 60)
        ),
        format!(
            "r6\t145\tc2\t201\t60\t50M\tc1\t901\t0\t{}\t*",
            r("c2", 201, 250)
        ),
        format!(
            "r7\t73\tc2\t301\t60\t50M\t=\t301\t0\t{}\t*",
            r("c2", 301, 350)
        ),
        format!(
            "r7\t133\tc2\t301\t0\t*\t=\t301\t0\t{}\t*",
            r("c2", 301, 350)
        ),
        "u1\t4\t*\t0\t0\t*\t*\t0\t0\tACGT\t*".to_string(),
    ];
    let mut text = String::from(
        "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c1\tLN:1000\n@SQ\tSN:c2\tLN:500\n@SQ\tSN:c3\tLN:300\n",
    );
    for row in rows {
        text.push_str(&row);
        text.push('\n');
    }
    text
}

/// Write the fixture's `reads.bam` (no index) and `ref.fa` (no `.fai`).
pub fn fixture(dir: &Path) -> Fixture {
    let bam = bam_from_sam(dir, "reads.bam", &fixture_sam());
    let reference = dir.join("ref.fa");
    let mut fasta = String::new();
    for c in ["c1", "c2", "c3"] {
        fasta.push_str(&format!(">{c}\n"));
        for line in reference_seq(c).as_bytes().chunks(60) {
            fasta.push_str(std::str::from_utf8(line).unwrap());
            fasta.push('\n');
        }
    }
    std::fs::write(&reference, fasta).expect("write ref.fa");
    Fixture { bam, reference }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_is_a_readable_sorted_bam() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let mut reader = bam::io::reader::Builder.build_from_path(&f.bam).unwrap();
        let header = reader.read_header().unwrap();
        assert_eq!(header.reference_sequences().len(), 3);
        let text =
            String::from_utf8(header.header().map(|h| format!("{h:?}")).unwrap().into()).unwrap();
        assert!(text.contains("coordinate"), "{text}");
        assert_eq!(reader.records().count(), 12);
        assert!(
            std::fs::read_to_string(&f.reference)
                .unwrap()
                .starts_with(">c1\nACGTACGT")
        );
    }
}
