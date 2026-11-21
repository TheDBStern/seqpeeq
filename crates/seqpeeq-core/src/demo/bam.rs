//! The example reference and BAMs.
//!
//! `reads.bam` plants every kind of read seqpeeq draws differently; positions
//! below are 1-based on `ref.fa`.

use std::fs::File;
use std::path::Path;

use noodles::bam;
use noodles::core::Position;
use noodles::sam;
use noodles::sam::alignment::RecordBuf;
use noodles::sam::alignment::io::Write as _;
use noodles::sam::alignment::record::cigar::Op;
use noodles::sam::alignment::record::cigar::op::Kind;
use noodles::sam::alignment::record::data::field::Tag;
use noodles::sam::alignment::record::{Flags, MappingQuality};
use noodles::sam::alignment::record_buf::data::field::Value;
use noodles::sam::alignment::record_buf::{Cigar, Data, Sequence};

use super::Lcg;

/// Which example BAM to write.
#[derive(Clone, Copy, Debug)]
pub enum Sample {
    /// `reads.bam`: every planted feature.
    Main,
    /// `reads2.bam`: plain pairs, for stacking with the main sample.
    Second,
}

const READ: u64 = 150;
/// `chr1` positions no read covers in the main sample.
const GAP: (u64, u64) = (12_001, 12_600);

/// The example reference: `chr1` (20,000 bp) and `chr2` (5,000 bp).
pub fn reference(seed: u64) -> Vec<(String, Vec<u8>)> {
    let mut rng = Lcg(seed);
    [("chr1", 20_000), ("chr2", 5_000)]
        .into_iter()
        .map(|(name, len)| (name.to_string(), (0..len).map(|_| rng.base()).collect()))
        .collect()
}

/// The sample's differences from the reference, all on `chr1`.
struct Variants {
    /// Every read carries these.
    hom: &'static [u64],
    /// About half the reads carry this one.
    het: Option<u64>,
    /// About half the reads carry these bases after this position.
    insertion: Option<(u64, &'static [u8])>,
    /// About a third of the reads lack these positions.
    deletion: Option<(u64, u64)>,
}

const MAIN: Variants = Variants {
    hom: &[1_020, 1_071],
    het: Some(1_045),
    insertion: Some((1_060, b"TA")),
    deletion: Some((1_085, 1_087)),
};

const SECOND: Variants = Variants {
    hom: &[1_033],
    het: None,
    insertion: None,
    deletion: None,
};

/// A different base: A↔G, C↔T.
fn alt(base: u8) -> u8 {
    match base {
        b'A' => b'G',
        b'G' => b'A',
        b'C' => b'T',
        _ => b'C',
    }
}

/// One alignment, before it is written.
struct Rec {
    name: String,
    flags: u16,
    contig: usize,
    pos: u64,
    mapq: u8,
    cigar: Vec<(Kind, usize)>,
    seq: Vec<u8>,
    mate: Option<(usize, u64)>,
    sa: Option<String>,
}

/// Add `len` of `kind` to a CIGAR, merging with the op before.
fn push(cigar: &mut Vec<(Kind, usize)>, kind: Kind, len: usize) {
    match cigar.last_mut() {
        Some((last, n)) if *last == kind => *n += len,
        _ => cigar.push((kind, len)),
    }
}

/// Reads of the sample, drawn from the reference with its variants.
struct Reads<'a> {
    rng: Lcg,
    reference: &'a [(String, Vec<u8>)],
    variants: &'a Variants,
}

impl Reads<'_> {
    /// `n` read bases aligned from `pos` on `contig`: the CIGAR, the bases
    /// and the start (moved past a deletion a read would start in).
    fn aligned(
        &mut self,
        contig: usize,
        mut pos: u64,
        n: u64,
    ) -> (Vec<(Kind, usize)>, Vec<u8>, u64) {
        let seq = &self.reference[contig].1;
        let chr1 = contig == 0;
        let v = self.variants;
        let het = chr1 && self.rng.range(0, 1) == 0;
        let ins = chr1 && self.rng.range(0, 1) == 0;
        let del = chr1 && self.rng.range(0, 2) == 0;
        if let (true, Some((a, b))) = (del, v.deletion)
            && (a..=b).contains(&pos)
        {
            pos = b + 1;
        }
        let (mut cigar, mut bases, mut at) = (Vec::new(), Vec::new(), pos);
        while (bases.len() as u64) < n {
            if let (true, Some((a, b))) = (del, v.deletion)
                && at == a
            {
                push(&mut cigar, Kind::Deletion, (b - a + 1) as usize);
                at = b + 1;
                continue;
            }
            let mut base = seq[(at - 1) as usize];
            if chr1 && (v.hom.contains(&at) || (het && v.het == Some(at))) {
                base = alt(base);
            }
            push(&mut cigar, Kind::Match, 1);
            bases.push(base);
            if let (true, Some((after, extra))) = (ins, v.insertion)
                && at == after
                && bases.len() as u64 + (extra.len() as u64) < n
            {
                push(&mut cigar, Kind::Insertion, extra.len());
                bases.extend_from_slice(extra);
            }
            at += 1;
        }
        (cigar, bases, pos)
    }

    fn random_bases(&mut self, n: u64) -> Vec<u8> {
        (0..n).map(|_| self.rng.base()).collect()
    }

    /// Plain proper pairs at about `depth` over `contig`, none touching
    /// `avoid`.
    fn pairs(&mut self, recs: &mut Vec<Rec>, contig: usize, depth: u64, avoid: Option<(u64, u64)>) {
        let len = self.reference[contig].1.len() as u64;
        let count = len * depth / (2 * READ);
        let base = recs.len();
        for i in 0..count {
            let fragment = self.rng.range(300, 500);
            let p = self.rng.range(1, len - fragment - 3);
            if let Some((a, b)) = avoid
                && p <= b
                && p + fragment + 3 >= a
            {
                continue;
            }
            let q = p + fragment - READ;
            let (c1, s1, p) = self.aligned(contig, p, READ);
            let (c2, s2, q) = self.aligned(contig, q, READ);
            let first_forward = self.rng.range(0, 1) == 0;
            let (f1, f2) = if first_forward { (99, 147) } else { (163, 83) };
            let name = format!("pair{}", base + i as usize);
            recs.push(rec(&name, f1, contig, p, c1, s1, Some((contig, q))));
            recs.push(rec(&name, f2, contig, q, c2, s2, Some((contig, p))));
        }
    }

    /// The main sample's special reads, around `chr1:1,000-1,100`.
    fn planted(&mut self, recs: &mut Vec<Rec>) {
        let chr2 = &self.reference[1].1;
        for n in 0..4u64 {
            let (p, at) = (1_005 + 5 * n, 2_001 + 3 * n);
            let (mut cigar, mut seq, p) = self.aligned(0, p, 90);
            seq.extend_from_slice(&chr2[(at - 1) as usize..(at - 1 + 60) as usize]);
            push(&mut cigar, Kind::SoftClip, 60);
            let name = format!("split{}", n + 1);
            let mut primary = rec(&name, 0, 0, p, cigar, seq.clone(), None);
            primary.sa = Some(format!("chr2,{at},+,90S60M,60,0;"));
            let mut part = rec(
                &name,
                2048,
                1,
                at,
                vec![(Kind::SoftClip, 90), (Kind::Match, 60)],
                seq,
                None,
            );
            part.sa = Some(format!("chr1,{p},+,90M60S,60,0;"));
            recs.extend([primary, part]);
        }
        for n in 0..3u64 {
            let name = format!("away{}", n + 1);
            let (c, s, p) = self.aligned(0, 1_000 + 15 * n, READ);
            let at = 3_001 + 20 * n;
            let (c2, s2, at) = self.aligned(1, at, READ);
            recs.push(rec(&name, 97, 0, p, c, s, Some((1, at))));
            recs.push(rec(&name, 145, 1, at, c2, s2, Some((0, p))));
        }
        for n in 0..3u64 {
            let name = format!("improper{}", n + 1);
            let (c, s, p) = self.aligned(0, 1_008 + 15 * n, READ);
            let (c2, s2, q) = self.aligned(0, p + 5_000, READ);
            recs.push(rec(&name, 97, 0, p, c, s, Some((0, q))));
            recs.push(rec(&name, 145, 0, q, c2, s2, Some((0, p))));
        }
        for n in 0..4u64 {
            let (aligned, mut seq, p) = self.aligned(0, 1_015 + 8 * n, READ - 20);
            let mut clip = self.random_bases(20);
            clip.append(&mut seq);
            let mut cigar = vec![(Kind::SoftClip, 20)];
            cigar.extend(aligned);
            recs.push(rec(&format!("clip{}", n + 1), 0, 0, p, cigar, clip, None));
        }
        for (n, (at, flags, mapq)) in [
            (1_012, 0, 0),
            (1_050, 16, 0),
            (1_030, 1024, 60),
            (1_040, 256, 60),
        ]
        .into_iter()
        .enumerate()
        {
            let (c, s, p) = self.aligned(0, at, READ);
            let mut r = rec(&format!("dim{}", n + 1), flags, 0, p, c, s, None);
            r.mapq = mapq;
            recs.push(r);
        }
        let chr1 = &self.reference[0].1;
        for n in 0..2u64 {
            let p = 8_001 + 10 * n;
            let mut seq = chr1[(p - 1) as usize..(p - 1 + 60) as usize].to_vec();
            let after = p + 60 + 800;
            seq.extend_from_slice(&chr1[(after - 1) as usize..(after - 1 + 90) as usize]);
            let cigar = vec![(Kind::Match, 60), (Kind::Skip, 800), (Kind::Match, 90)];
            recs.push(rec(&format!("spliced{}", n + 1), 0, 0, p, cigar, seq, None));
        }
    }
}

fn rec(
    name: &str,
    flags: u16,
    contig: usize,
    pos: u64,
    cigar: Vec<(Kind, usize)>,
    seq: Vec<u8>,
    mate: Option<(usize, u64)>,
) -> Rec {
    Rec {
        name: name.to_string(),
        flags,
        contig,
        pos,
        mapq: 60,
        cigar,
        seq,
        mate,
        sa: None,
    }
}

fn header(reference: &[(String, Vec<u8>)]) -> sam::Header {
    let mut text = String::from("@HD\tVN:1.6\tSO:coordinate\n");
    for (name, seq) in reference {
        text.push_str(&format!("@SQ\tSN:{name}\tLN:{}\n", seq.len()));
    }
    text.parse().expect("a SAM header")
}

fn position(p: u64) -> Position {
    Position::try_from(p as usize).expect("a position from 1")
}

/// Write the BAM for `sample`, aligned to `reference`, with no index.
pub fn write_reads(
    path: &Path,
    reference: &[(String, Vec<u8>)],
    sample: Sample,
) -> std::io::Result<()> {
    let (seed, variants, depths, avoid) = match sample {
        Sample::Main => (0x5eed_b001, &MAIN, [20, 10], Some(GAP)),
        Sample::Second => (0x5eed_b002, &SECOND, [8, 8], None),
    };
    let mut reads = Reads {
        rng: Lcg(seed),
        reference,
        variants,
    };
    let mut recs = Vec::new();
    reads.pairs(&mut recs, 0, depths[0], avoid);
    reads.pairs(&mut recs, 1, depths[1], None);
    if let Sample::Main = sample {
        reads.planted(&mut recs);
    }
    recs.sort_by(|a, b| (a.contig, a.pos, &a.name).cmp(&(b.contig, b.pos, &b.name)));

    let header = header(reference);
    let mut writer = bam::io::Writer::new(File::create(path)?);
    writer.write_header(&header)?;
    for r in &recs {
        let mut builder = RecordBuf::builder()
            .set_name(r.name.as_str())
            .set_flags(Flags::from(r.flags))
            .set_reference_sequence_id(r.contig)
            .set_alignment_start(position(r.pos))
            .set_mapping_quality(MappingQuality::new(r.mapq).expect("a mapping quality"))
            .set_cigar(
                r.cigar
                    .iter()
                    .map(|&(kind, len)| Op::new(kind, len))
                    .collect::<Cigar>(),
            )
            .set_sequence(Sequence::from(r.seq.clone()));
        if let Some((contig, at)) = r.mate {
            builder = builder
                .set_mate_reference_sequence_id(contig)
                .set_mate_alignment_start(position(at));
        }
        if let Some(sa) = &r.sa {
            let data: Data = [(Tag::OTHER_ALIGNMENTS, Value::String(sa.as_str().into()))]
                .into_iter()
                .collect();
            builder = builder.set_data(data);
        }
        writer.write_alignment_record(&header, &builder.build())?;
    }
    writer.try_finish()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use noodles::bam;
    use noodles::sam;
    use noodles::sam::alignment::RecordBuf;
    use noodles::sam::alignment::record::cigar::op::Kind;
    use noodles::sam::alignment::record::data::field::Tag;
    use std::path::PathBuf;

    /// A BAM's header and records.
    pub(crate) fn records(path: &Path) -> (sam::Header, Vec<RecordBuf>) {
        let mut reader = bam::io::Reader::new(std::fs::File::open(path).expect("open the BAM"));
        let header = reader.read_header().expect("a BAM header");
        let records = reader
            .record_bufs(&header)
            .collect::<std::io::Result<Vec<_>>>()
            .expect("BAM records");
        (header, records)
    }

    fn written(sample: Sample) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.bam");
        write_reads(&path, &reference(REF_SEED), sample).unwrap();
        (dir, path)
    }

    const REF_SEED: u64 = 0x5eed_0e01;

    fn start(r: &RecordBuf) -> u64 {
        r.alignment_start().unwrap().get() as u64
    }

    /// The last reference position the record covers.
    fn end(r: &RecordBuf) -> u64 {
        let span: usize = r
            .cigar()
            .as_ref()
            .iter()
            .filter(|op| op.kind().consumes_reference())
            .map(|op| op.len())
            .sum();
        start(r) + span as u64 - 1
    }

    /// The read's base aligned to reference position `pos`, if any.
    fn base_at(r: &RecordBuf, pos: u64) -> Option<u8> {
        let (mut at, mut i) = (start(r), 0usize);
        let seq = r.sequence().as_ref();
        for op in r.cigar().as_ref() {
            let len = op.len();
            match op.kind() {
                Kind::Match | Kind::SequenceMatch | Kind::SequenceMismatch => {
                    if (at..at + len as u64).contains(&pos) {
                        return Some(seq[i + (pos - at) as usize]);
                    }
                    at += len as u64;
                    i += len;
                }
                Kind::Insertion | Kind::SoftClip => i += len,
                Kind::Deletion | Kind::Skip => at += len as u64,
                _ => {}
            }
        }
        None
    }

    fn has_op(r: &RecordBuf, kind: Kind) -> bool {
        r.cigar().as_ref().iter().any(|op| op.kind() == kind)
    }

    #[test]
    fn main_sample_plants_every_feature() {
        let (_dir, path) = written(Sample::Main);
        let (_, recs) = records(&path);
        let reference = reference(REF_SEED);
        let chr1 = |r: &&RecordBuf| r.reference_sequence_id() == Some(0);

        assert!(
            !recs
                .iter()
                .filter(chr1)
                .any(|r| start(r) <= 12_600 && end(r) >= 12_001),
            "a read in the gap"
        );
        let supplementary: Vec<_> = recs
            .iter()
            .filter(|r| r.flags().is_supplementary())
            .collect();
        assert!(supplementary.len() >= 4);
        for part in &supplementary {
            assert!(part.data().get(&Tag::OTHER_ALIGNMENTS).is_some());
            let primary = recs
                .iter()
                .find(|r| r.name() == part.name() && !r.flags().is_supplementary())
                .expect("the primary part");
            assert!(primary.data().get(&Tag::OTHER_ALIGNMENTS).is_some());
            assert_ne!(
                primary.reference_sequence_id(),
                part.reference_sequence_id()
            );
        }
        let paired = |r: &&RecordBuf| r.flags().is_segmented();
        assert!(
            recs.iter()
                .filter(paired)
                .filter(|r| r.mate_reference_sequence_id() != r.reference_sequence_id())
                .count()
                >= 3
        );
        assert!(
            recs.iter()
                .filter(paired)
                .filter(|r| r.mate_reference_sequence_id() == r.reference_sequence_id())
                .filter(|r| !r.flags().is_properly_segmented())
                .count()
                >= 3
        );
        let insertion_after_1060 = recs.iter().filter(chr1).any(|r| {
            let mut at = start(r);
            r.cigar().as_ref().iter().any(|op| {
                let here = op.kind() == Kind::Insertion && at == 1_061;
                if op.kind().consumes_reference() {
                    at += op.len() as u64;
                }
                here
            })
        });
        assert!(insertion_after_1060);
        assert!(recs.iter().any(|r| has_op(r, Kind::Deletion)));
        assert!(recs.iter().any(|r| has_op(r, Kind::Skip)));
        assert!(recs.iter().any(|r| {
            r.cigar()
                .as_ref()
                .first()
                .is_some_and(|op| op.kind() == Kind::SoftClip && op.len() >= 20)
                && !r.flags().is_supplementary()
        }));
        assert!(
            recs.iter()
                .any(|r| r.mapping_quality().map(|q| q.get()) == Some(0))
        );
        assert!(recs.iter().any(|r| r.flags().is_duplicate()));
        assert!(recs.iter().any(|r| r.flags().is_secondary()));

        let ref_1020 = reference[0].1[1_019];
        let covering: Vec<u8> = recs
            .iter()
            .filter(chr1)
            .filter_map(|r| base_at(r, 1_020))
            .collect();
        assert!(
            covering.len() >= 10,
            "{} reads cover chr1:1,020",
            covering.len()
        );
        assert!(covering.iter().all(|&b| b != ref_1020));
        // The heterozygous SNP shows in some reads only.
        let at_1045: Vec<u8> = recs
            .iter()
            .filter(chr1)
            .filter_map(|r| base_at(r, 1_045))
            .collect();
        let alt = at_1045
            .iter()
            .filter(|&&b| b != reference[0].1[1_044])
            .count();
        assert!(alt > 0 && alt < at_1045.len(), "{alt} of {}", at_1045.len());
    }

    #[test]
    fn records_are_coordinate_sorted() {
        for sample in [Sample::Main, Sample::Second] {
            let (_dir, path) = written(sample);
            let (_, recs) = records(&path);
            assert!(!recs.is_empty());
            let keys: Vec<_> = recs
                .iter()
                .map(|r| (r.reference_sequence_id(), start(r)))
                .collect();
            assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{sample:?}");
        }
    }

    #[test]
    fn samples_share_a_header() {
        let (_a, main) = written(Sample::Main);
        let (_b, second) = written(Sample::Second);
        let contigs = |p: &Path| {
            records(p)
                .0
                .reference_sequences()
                .iter()
                .map(|(name, seq)| (name.to_string(), seq.length().get()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            contigs(&main),
            vec![("chr1".into(), 20_000), ("chr2".into(), 5_000)]
        );
        assert_eq!(contigs(&main), contigs(&second));
    }

    #[test]
    fn write_all_is_deterministic() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        crate::demo::write_all(a.path()).unwrap();
        crate::demo::write_all(b.path()).unwrap();
        for name in ["aln.fa", "tree.nwk", "ref.fa"] {
            let read = |d: &Path| std::fs::read(d.join(name)).unwrap();
            assert!(!read(a.path()).is_empty(), "{name}");
            assert_eq!(read(a.path()), read(b.path()), "{name}");
        }
        for name in ["reads.bam", "reads2.bam"] {
            assert_eq!(
                records(&a.path().join(name)),
                records(&b.path().join(name)),
                "{name}"
            );
        }
        let size: u64 = crate::demo::FILES
            .iter()
            .map(|f| std::fs::metadata(a.path().join(f)).unwrap().len())
            .sum();
        assert!(size < 200_000, "{size} bytes");
    }
}
