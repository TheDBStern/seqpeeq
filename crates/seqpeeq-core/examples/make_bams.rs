//! Make two large BAMs for trying the BAM core at scale, then time it.
//!
//! cargo run --release -p seqpeeq-core --example make_bams -- testdata [--scale N]
//!
//! - `deep.bam` + `deep.fa`: one contig, `deep`, 5,000,000 × N bp, with 150 bp
//!   pairs at about 30× and nothing at 4,000,001-4,000,500.
//! - `assembly.bam` + `assembly.fa`: 5,000 contigs of 2-200 kb (log-uniform),
//!   a quarter of them with a few 5-50 kb reads; a tenth of the reads are split,
//!   with a supplementary part on another contig.
//!
//! The output is the same on every run (a fixed-seed LCG). Existing files are
//! replaced.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use noodles::bam;
use noodles::core::Position;
use noodles::sam;
use noodles::sam::alignment::RecordBuf;
use noodles::sam::alignment::io::Write as _;
use noodles::sam::alignment::record::cigar::Op;
use noodles::sam::alignment::record::cigar::op::Kind as OpKind;
use noodles::sam::alignment::record::data::field::Tag;
use noodles::sam::alignment::record::{Flags, MappingQuality};
use noodles::sam::alignment::record_buf::data::field::Value;
use noodles::sam::alignment::record_buf::{Cigar, Data, Sequence};

use seqpeeq_core::bam::{Direction, Stretch};
use seqpeeq_core::{Command, Progress, Session};

/// A 64-bit linear congruential generator (Knuth's MMIX constants).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }

    /// A number in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    fn base(&mut self) -> u8 {
        b"ACGT"[(self.next() >> 7) as usize % 4]
    }
}

fn bases(rng: &mut Lcg, n: u64) -> Vec<u8> {
    (0..n).map(|_| rng.base()).collect()
}

fn write_fasta(path: &Path, contigs: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    for (name, seq) in contigs {
        writeln!(out, ">{name}")?;
        for line in seq.chunks(60) {
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()
}

fn header(contigs: &[(String, Vec<u8>)]) -> sam::Header {
    let mut text = String::from("@HD\tVN:1.6\tSO:coordinate\n");
    for (name, seq) in contigs {
        text.push_str(&format!("@SQ\tSN:{name}\tLN:{}\n", seq.len()));
    }
    text.parse().expect("a SAM header")
}

/// One alignment, before it is written.
struct Rec {
    name: String,
    flags: u16,
    contig: usize,
    pos: u64,
    cigar: Vec<(OpKind, usize)>,
    seq: Vec<u8>,
    mate: Option<(usize, u64)>,
    sa: Option<String>,
}

fn pos(p: u64) -> Position {
    Position::try_from(p as usize).expect("a position from 1")
}

fn write_bam(path: &Path, header: &sam::Header, recs: &mut [Rec]) -> std::io::Result<()> {
    recs.sort_by_key(|r| (r.contig, r.pos));
    let mut writer = bam::io::Writer::new(File::create(path)?);
    writer.write_header(header)?;
    for r in recs.iter() {
        let mut builder = RecordBuf::builder()
            .set_name(r.name.as_str())
            .set_flags(Flags::from(r.flags))
            .set_reference_sequence_id(r.contig)
            .set_alignment_start(pos(r.pos))
            .set_mapping_quality(MappingQuality::new(60).expect("a mapping quality"))
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
                .set_mate_alignment_start(pos(at));
        }
        if let Some(sa) = &r.sa {
            let data: Data = [(Tag::OTHER_ALIGNMENTS, Value::String(sa.as_str().into()))]
                .into_iter()
                .collect();
            builder = builder.set_data(data);
        }
        writer.write_alignment_record(header, &builder.build())?;
    }
    writer.try_finish()
}

fn deep(dir: &Path, scale: u64) -> std::io::Result<PathBuf> {
    let mut rng = Lcg(0x5eed_0001);
    let length = 5_000_000 * scale;
    let reference = bases(&mut rng, length);
    let (gap_start, gap_end) = (4_000_001, 4_000_500);
    let (read, fragment) = (150u64, 400u64);
    let pairs = length * 30 / (2 * read);
    let mut recs = Vec::with_capacity(pairs as usize * 2);
    for i in 0..pairs {
        let p = rng.range(1, length - fragment + 1);
        if p <= gap_end && p + fragment > gap_start {
            continue;
        }
        let q = p + fragment - read;
        let name = format!("p{i}");
        let seq = |at: u64| reference[(at - 1) as usize..(at - 1 + read) as usize].to_vec();
        recs.push(Rec {
            name: name.clone(),
            flags: 99,
            contig: 0,
            pos: p,
            cigar: vec![(OpKind::Match, read as usize)],
            seq: seq(p),
            mate: Some((0, q)),
            sa: None,
        });
        recs.push(Rec {
            name,
            flags: 147,
            contig: 0,
            pos: q,
            cigar: vec![(OpKind::Match, read as usize)],
            seq: seq(q),
            mate: Some((0, p)),
            sa: None,
        });
    }
    let contigs = vec![("deep".to_string(), reference)];
    write_fasta(&dir.join("deep.fa"), &contigs)?;
    let path = dir.join("deep.bam");
    write_bam(&path, &header(&contigs), &mut recs)?;
    Ok(path)
}

fn assembly(dir: &Path) -> std::io::Result<PathBuf> {
    let mut rng = Lcg(0x5eed_0002);
    let contigs: Vec<(String, Vec<u8>)> = (0..5_000)
        .map(|i| {
            // Log-uniform from 2 kb to 200 kb.
            let f = (rng.next() % 1_000_000) as f64 / 1_000_000.0;
            let len = (2_000.0 * 100f64.powf(f)) as u64;
            (format!("contig_{:04}", i + 1), bases(&mut rng, len))
        })
        .collect();
    let mut recs = Vec::new();
    let mut n = 0;
    for (c, (_, seq)) in contigs.iter().enumerate() {
        if !rng.next().is_multiple_of(4) {
            continue;
        }
        let length = seq.len() as u64;
        for _ in 0..rng.range(1, 8) {
            n += 1;
            let len = rng.range(5_000, 50_000).min(length);
            let p = rng.range(1, length - len + 1);
            let aligned = seq[(p - 1) as usize..(p - 1 + len) as usize].to_vec();
            let name = format!("long{n}");
            let split = rng.next().is_multiple_of(10);
            if !split {
                recs.push(Rec {
                    name,
                    flags: 0,
                    contig: c,
                    pos: p,
                    cigar: vec![(OpKind::Match, len as usize)],
                    seq: aligned,
                    mate: None,
                    sa: None,
                });
                continue;
            }
            // The read goes on into another contig: 2-5 kb of it aligns there.
            let other = (c + 1 + rng.range(0, 4_998) as usize) % contigs.len();
            let other_seq = &contigs[other].1;
            let tail = rng.range(2_000, 5_000).min(other_seq.len() as u64);
            let at = rng.range(1, other_seq.len() as u64 - tail + 1);
            let tail_seq = &other_seq[(at - 1) as usize..(at - 1 + tail) as usize];
            let mut whole = aligned.clone();
            whole.extend_from_slice(tail_seq);
            let (name_c, name_o) = (&contigs[c].0, &contigs[other].0);
            recs.push(Rec {
                name: name.clone(),
                flags: 0,
                contig: c,
                pos: p,
                cigar: vec![
                    (OpKind::Match, len as usize),
                    (OpKind::SoftClip, tail as usize),
                ],
                seq: whole,
                mate: None,
                sa: Some(format!("{name_o},{at},+,{len}H{tail}M,60,0;")),
            });
            recs.push(Rec {
                name,
                flags: 2048,
                contig: other,
                pos: at,
                cigar: vec![
                    (OpKind::HardClip, len as usize),
                    (OpKind::Match, tail as usize),
                ],
                seq: tail_seq.to_vec(),
                mate: None,
                sa: Some(format!("{name_c},{p},+,{len}M{tail}S,60,0;")),
            });
        }
    }
    write_fasta(&dir.join("assembly.fa"), &contigs)?;
    let path = dir.join("assembly.bam");
    write_bam(&path, &header(&contigs), &mut recs)?;
    Ok(path)
}

fn time<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    println!("{what:<44} {:>9.3} s", start.elapsed().as_secs_f64());
    out
}

fn open(path: &Path, cache: &Path) -> Session {
    let mut s = Session::new(None);
    s.set_cache_dir(cache.to_path_buf());
    let out = s
        .execute(Command::Open {
            path: path.to_path_buf(),
            kind: None,
        })
        .expect("the BAM opens");
    println!("  {}", out.message.replace('\n', "; "));
    s
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(args.first().map_or("testdata", String::as_str));
    let scale = match args.iter().position(|a| a == "--scale") {
        Some(i) => args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(1),
        None => 1,
    };
    std::fs::create_dir_all(&dir)?;
    for stale in [
        "deep.bam.bai",
        "deep.bam.csi",
        "assembly.bam.bai",
        "assembly.bam.csi",
    ] {
        let _ = std::fs::remove_file(dir.join(stale));
    }
    let cache = std::env::temp_dir().join("seqpeeq-make-bams-cache");
    let _ = std::fs::remove_dir_all(&cache);

    let deep = time("write deep.bam + deep.fa", || deep(&dir, scale))?;
    let assembly = time("write assembly.bam + assembly.fa", || assembly(&dir))?;
    time("deep: open, building the index", || open(&deep, &cache));
    time("assembly: open, building the index", || {
        open(&assembly, &cache)
    });
    time("assembly: open with its index", || open(&assembly, &cache));
    let s = time("deep: open with its index", || open(&deep, &cache));
    let source = s.datasets()[0].bam().expect("a BAM").source();
    let progress = Progress::new();
    time("deep: sketch, 200 bins", || source.sketch(0, 200));
    let coverage = time("deep: coverage pass, 200 bins", || {
        source
            .coverage(0, 200, &progress, |_| {})
            .expect("coverage")
    });
    println!(
        "  {} covered stretches; mean depth {:.1}",
        coverage.covered.len(),
        coverage.bins.iter().sum::<f32>() / coverage.bins.len() as f32
    );
    let found = time("deep: jump from 1 to the next empty stretch", || {
        source
            .jump(0, 1, Direction::Forward, Stretch::Empty, &progress)
            .expect("a jump")
    });
    println!("  found {found:?}");
    let found = time("deep: jump back from the end to empty", || {
        source
            .jump(
                0,
                5_000_000 * scale,
                Direction::Back,
                Stretch::Empty,
                &progress,
            )
            .expect("a jump")
    });
    println!("  found {found:?}");
    Ok(())
}
