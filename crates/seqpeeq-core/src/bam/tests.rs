//! BAM files through the session: opening, indexes, commands.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use noodles::bam;
use noodles::csi;
use noodles::csi::BinningIndex as _;
use noodles::csi::binning_index::ReferenceSequence as _;

use super::testing::{bam_from_sam, fixture};
use crate::{Command, Error, Outcome, Session, Started};

/// A session with no log whose cache folder is `cache`.
fn session(cache: &Path) -> Session {
    let mut s = Session::new(None);
    s.set_cache_dir(cache.to_path_buf());
    s
}

fn open(s: &mut Session, path: &Path) -> crate::Result<Outcome> {
    s.execute(Command::Open {
        path: path.to_path_buf(),
        kind: None,
    })
}

fn ex(s: &mut Session, line: &str) -> crate::Result<Outcome> {
    s.execute(Command::parse_line(line).unwrap())
}

/// Two temp folders: the data, and the cache.
fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
}

const COUNTS: &str = "3 contigs, 10 mapped reads, 2 unmapped";

#[test]
fn a_bam_opens_with_counts_and_a_new_index_beside_it() {
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    let out = open(&mut s, &f.bam).unwrap();
    assert_eq!(
        out.message,
        format!(
            "opened {} (bam, {COUNTS}); built the index reads.bam.bai",
            f.bam.display()
        )
    );
    assert!(data.path().join("reads.bam.bai").exists());
    assert_eq!(s.datasets()[0].bam().unwrap().shape(), COUNTS);
    assert_eq!(
        ex(&mut s, "undo").unwrap_err().to_string(),
        "a BAM has no edits to undo"
    );
    assert_eq!(
        ex(&mut s, "redo").unwrap_err().to_string(),
        "a BAM has no edits to redo"
    );

    let mut again = session(cache.path());
    let out = open(&mut again, &f.bam).unwrap();
    assert_eq!(
        out.message,
        format!("opened {} (bam, {COUNTS})", f.bam.display())
    );
}

/// Whether files can still be created in `dir` (as under root, where a
/// read-only folder is not read-only).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    let made = File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    made
}

#[cfg(unix)]
#[test]
fn an_index_goes_to_the_cache_when_the_folder_is_read_only() {
    use std::os::unix::fs::PermissionsExt;
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let set = |mode| fs::set_permissions(data.path(), fs::Permissions::from_mode(mode)).unwrap();
    set(0o555);
    if writable(data.path()) {
        set(0o755);
        eprintln!("skipped: the folder stays writable (running as root?)");
        return;
    }
    let mut s = session(cache.path());
    let out = open(&mut s, &f.bam);
    let mut again = session(cache.path());
    let second = open(&mut again, &f.bam);
    set(0o755);
    let out = out.unwrap();
    let cached: Vec<PathBuf> = fs::read_dir(cache.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(cached.len(), 1, "{cached:?}");
    let name = cached[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("reads.bam-") && name.ends_with(".bai") && name.len() == 10 + 16 + 4,
        "{name}"
    );
    assert!(
        out.message.ends_with(&format!(
            "; reads.bam's folder is read-only, so the index went to {}",
            cached[0].display()
        )),
        "{}",
        out.message
    );
    assert_eq!(
        second.unwrap().message,
        format!("opened {} (bam, {COUNTS})", f.bam.display())
    );
}

#[test]
fn an_index_older_than_its_bam_is_rebuilt_in_the_cache() {
    let (data, cache) = dirs();
    let f = fixture(data.path());
    open(&mut session(cache.path()), &f.bam).unwrap();
    let bai = data.path().join("reads.bam.bai");
    let before = fs::read(&bai).unwrap();
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    File::options()
        .write(true)
        .open(&bai)
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    let out = open(&mut session(cache.path()), &f.bam).unwrap();
    assert!(
        out.message
            .ends_with("; reads.bam.bai is older than reads.bam; built a fresh index in the cache"),
        "{}",
        out.message
    );
    assert_eq!(fs::read(&bai).unwrap(), before, "left untouched");
    assert_eq!(fs::read_dir(cache.path()).unwrap().count(), 1);
    // The fresh one in the cache is used from then on, silently.
    let out = open(&mut session(cache.path()), &f.bam).unwrap();
    assert_eq!(
        out.message,
        format!("opened {} (bam, {COUNTS})", f.bam.display())
    );
}

#[test]
fn other_index_names_are_found() {
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let index = bam::fs::index(&f.bam).unwrap();
    let bam::Index::Bai(bai) = index.clone() else {
        panic!("a small contig gets a BAI");
    };
    bam::bai::fs::write(data.path().join("reads.bai"), &bai).unwrap();
    let out = open(&mut session(cache.path()), &f.bam).unwrap();
    assert_eq!(
        out.message,
        format!("opened {} (bam, {COUNTS})", f.bam.display())
    );
    fs::remove_file(data.path().join("reads.bai")).unwrap();

    let csi_index: csi::Index = csi::binning_index::Index::builder()
        .set_min_shift(14)
        .set_depth(5)
        .set_reference_sequences(
            bai.reference_sequences()
                .to_vec()
                .into_iter()
                .map(|r| {
                    csi::binning_index::index::ReferenceSequence::new(
                        r.bins().clone(),
                        Default::default(),
                        r.metadata().cloned(),
                    )
                })
                .collect(),
        )
        .set_unplaced_unmapped_record_count(bai.unplaced_unmapped_record_count().unwrap_or(0))
        .build();
    csi::fs::write(data.path().join("reads.bam.csi"), &csi_index).unwrap();
    let out = open(&mut session(cache.path()), &f.bam).unwrap();
    assert_eq!(
        out.message,
        format!("opened {} (bam, {COUNTS})", f.bam.display())
    );
    assert!(!data.path().join("reads.bam.bai").exists());
}

const UNSORTED: &str = "reads.bam is not sorted by position (samtools sort makes a sorted copy)";

#[test]
fn unsorted_bams_are_refused() {
    let (data, cache) = dirs();
    let read = |pos: u64| format!("r{pos}\t0\tc1\t{pos}\t60\t4M\t*\t0\t0\tACGT\t*\n");
    let sq = "@SQ\tSN:c1\tLN:1000\n";

    let queryname = format!("@HD\tVN:1.6\tSO:queryname\n{sq}{}", read(5));
    let path = bam_from_sam(data.path(), "reads.bam", &queryname);
    let err = open(&mut session(cache.path()), &path).unwrap_err();
    assert_eq!(err.to_string(), UNSORTED);

    fs::remove_file(&path).unwrap();
    let out_of_order = format!("@HD\tVN:1.6\tSO:coordinate\n{sq}{}{}", read(50), read(5));
    let path = bam_from_sam(data.path(), "reads.bam", &out_of_order);
    let err = open(&mut session(cache.path()), &path).unwrap_err();
    assert_eq!(err.to_string(), UNSORTED);
    let left: Vec<_> = fs::read_dir(data.path()).unwrap().collect();
    assert_eq!(left.len(), 1, "no index left behind");

    fs::remove_file(&path).unwrap();
    let no_order = format!("@HD\tVN:1.6\n{sq}{}{}", read(5), read(50));
    let path = bam_from_sam(data.path(), "reads.bam", &no_order);
    open(&mut session(cache.path()), &path).unwrap();
    assert!(data.path().join("reads.bam.bai").exists());
}

#[test]
fn a_contig_past_2_29_gets_a_csi_index() {
    let (data, cache) = dirs();
    let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:big\tLN:536870913\n\
               r1\t0\tbig\t536870000\t60\t4M\t*\t0\t0\tACGT\t*\n";
    let path = bam_from_sam(data.path(), "big.bam", sam);
    let out = open(&mut session(cache.path()), &path).unwrap();
    assert!(
        out.message.ends_with("; built the index big.bam.csi"),
        "{}",
        out.message
    );
    assert!(data.path().join("big.bam.csi").exists());
}

#[test]
fn an_index_without_read_counts_still_opens() {
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let bam::Index::Bai(bai) = bam::fs::index(&f.bam).unwrap() else {
        panic!("a BAI");
    };
    let bare = bam::bai::Index::builder()
        .set_reference_sequences(
            bai.reference_sequences()
                .iter()
                .map(|r| {
                    csi::binning_index::index::ReferenceSequence::new(
                        r.bins().clone(),
                        r.index().clone(),
                        None,
                    )
                })
                .collect(),
        )
        .build();
    bam::bai::fs::write(data.path().join("reads.bam.bai"), &bare).unwrap();
    let out = open(&mut session(cache.path()), &f.bam).unwrap();
    assert_eq!(
        out.message,
        format!(
            "opened {} (bam, 3 contigs; the index has no read counts)",
            f.bam.display()
        )
    );
}

#[test]
fn cancelling_an_index_build_leaves_nothing() {
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    let Started::Running(job) = s
        .start(Command::Open {
            path: f.bam.clone(),
            kind: None,
        })
        .unwrap()
    else {
        panic!("opening a BAM is a job");
    };
    job.progress().cancel();
    let err = s.finish(job.run()).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(!data.path().join("reads.bam.bai").exists());
    assert!(s.datasets().is_empty());
}

/// The fixture opened directly, with its index built in `data`.
fn fixture_source(data: &Path, cache: &Path) -> super::BamSource {
    let f = fixture(data);
    super::BamDoc::open(&f.bam, cache, &crate::Progress::new())
        .unwrap()
        .source()
        .clone()
}

fn region(source: &super::BamSource, text: &str) -> super::Region {
    super::parse_region(text, source.contigs(), "reads.bam")
        .unwrap()
        .0
}

#[test]
fn reads_carry_what_the_view_needs() {
    use super::{Part, Place};
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    let reads = |text: &str| source.reads(&region(&source, text), &progress).unwrap();
    let names = |rs: &[super::Read]| rs.iter().map(|r| r.name.clone()).collect::<Vec<_>>();

    let rs = reads("c1:101-160");
    assert_eq!(names(&rs), ["r1", "r2"]);
    let (r1, r2) = (&rs[0], &rs[1]);
    assert_eq!(
        r1.mate,
        Some(Place {
            contig: 0,
            pos: 301,
            reverse: true
        })
    );
    assert_eq!(r1.md.as_deref(), Some("9C40"));
    assert_eq!((r1.start, r1.end, r1.contig, r1.mapq), (101, 150, 0, 60));
    assert_eq!(r2.blocks(), [(121, 150), (156, 165)]);
    assert_eq!(
        r2.parts,
        [Part {
            place: Place {
                contig: 1,
                pos: 51,
                reverse: false
            },
            cigar: "10M40H".to_string(),
            mapq: 60,
            nm: Some(0)
        }]
    );
    assert_eq!(r2.cigar[0], ('S', 10));
    assert_eq!(r2.seq.len(), 50);
    assert!(!r2.reverse());

    let rs = reads("c1:701-760");
    assert_eq!(names(&rs), ["r3", "r4", "r5"]);
    assert_eq!(
        rs.iter().map(|r| r.counts()).collect::<Vec<_>>(),
        [true, false, false]
    );
    assert!(rs[1].seq.is_empty(), "SEQ * reads as no bases");

    let r6 = &reads("c1:901-950")[0];
    assert_eq!(
        r6.mate,
        Some(Place {
            contig: 1,
            pos: 201,
            reverse: true
        })
    );
    assert!(reads("c2:201-250")[0].reverse());

    let rs = reads("c2:301-350");
    assert_eq!(names(&rs), ["r7"]);
    assert_eq!(rs[0].flags, 73, "the placed unmapped mate is skipped");
    assert!(reads("c3").is_empty());
}

#[test]
fn split_parts_on_unknown_contigs_are_dropped() {
    let (data, cache) = dirs();
    let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c1\tLN:1000\n\
               r1\t0\tc1\t5\t60\t4M\t*\t0\t0\tACGT\t*\tSA:Z:nowhere,9,-,4M,60,0;c1,50,-,4M,30,0;\n";
    let path = bam_from_sam(data.path(), "s.bam", sam);
    let doc = super::BamDoc::open(&path, cache.path(), &crate::Progress::new()).unwrap();
    let source = doc.source();
    let reads = source
        .reads(&region(source, "c1"), &crate::Progress::new())
        .unwrap();
    assert_eq!(reads[0].parts.len(), 1);
    assert_eq!(reads[0].parts[0].place.pos, 50);
    assert!(reads[0].parts[0].place.reverse);
    assert_eq!(reads[0].parts[0].mapq, 30);
}

#[test]
fn jumps_read_the_bam_and_agree_with_next_start() {
    use super::{Direction, Stretch, next_start};
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    type Contig = (usize, u64, Vec<(u64, u64)>);
    let covered: [Contig; 3] = [
        (
            0,
            1000,
            vec![(101, 165), (301, 350), (701, 740), (901, 950)],
        ),
        (1, 500, vec![(51, 60), (201, 250), (301, 350)]),
        (2, 300, vec![]),
    ];
    for (contig, length, stretches) in &covered {
        for from in 1..=*length {
            for dir in [Direction::Forward, Direction::Back] {
                for want in [Stretch::Covered, Stretch::Empty] {
                    let expected = next_start(stretches, *length, from, dir, want);
                    let got = source.jump(*contig, from, dir, want, &progress).unwrap();
                    assert_eq!(
                        got, expected,
                        "contig {contig} from {from} {dir:?} {want:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_cancelled_jump_stops() {
    use super::{Direction, Stretch};
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    progress.cancel();
    let err = source
        .jump(0, 1, Direction::Forward, Stretch::Covered, &progress)
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

#[test]
fn the_pass_gives_mean_depth_and_exact_stretches() {
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    let mut seen = Vec::new();
    let c1 = source
        .coverage(0, 10, &progress, |partial| seen.push(partial.clone()))
        .unwrap();
    assert_eq!(c1.bins, [0.0, 0.9, 0.0, 0.5, 0.0, 0.0, 0.0, 0.4, 0.0, 0.5]);
    assert_eq!(c1.covered, [(101, 165), (301, 350), (701, 740), (901, 950)]);
    assert_eq!((c1.contig, c1.done_to), (0, 1000));
    assert_eq!(seen.last(), Some(&c1));

    let c3 = source.coverage(2, 3, &progress, |_| {}).unwrap();
    assert_eq!(c3.bins, [0.0, 0.0, 0.0]);
    assert!(c3.covered.is_empty());

    progress.cancel();
    let err = source.coverage(0, 10, &progress, |_| {}).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

#[test]
fn the_sketch_shows_where_reads_are_from_the_index_alone() {
    let (data, cache) = dirs();
    let mut sam = String::from("@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:w\tLN:100000\n");
    for i in 0..20 {
        sam.push_str(&format!(
            "r{i}\t0\tw\t{}\t60\t100M\t*\t0\t0\t{}\t*\n",
            70_001 + i,
            "ACGT".repeat(25)
        ));
    }
    let path = bam_from_sam(data.path(), "w.bam", &sam);
    let doc = super::BamDoc::open(&path, cache.path(), &crate::Progress::new()).unwrap();
    let sketch = doc.source().sketch(0, 4);
    assert_eq!(
        (sketch[0], sketch[1], sketch[2]),
        (0.0, 0.0, 1.0),
        "{sketch:?}"
    );

    let source = fixture_source(data.path(), cache.path());
    assert_eq!(source.sketch(2, 5), [0.0; 5]);
}

#[test]
fn coverage_answers_jumps_only_where_it_is_final() {
    use super::{Coverage, Direction, Stretch};
    let partial = Coverage {
        contig: 0,
        bins: vec![],
        covered: vec![(101, 150), (156, 165), (301, 350)],
        done_to: 400,
    };
    let n = |from, dir, want| partial.next_start(1000, from, dir, want);
    assert_eq!(n(120, Direction::Forward, Stretch::Covered), Some(156));
    assert_eq!(
        n(360, Direction::Forward, Stretch::Covered),
        None,
        "701 is not known yet"
    );
    assert_eq!(n(320, Direction::Forward, Stretch::Empty), Some(351));
    assert_eq!(n(360, Direction::Forward, Stretch::Empty), None);
    assert_eq!(n(380, Direction::Back, Stretch::Covered), Some(301));
    assert_eq!(
        n(600, Direction::Back, Stretch::Covered),
        None,
        "past what is known"
    );
}

#[test]
fn the_pass_hands_out_partial_results_that_are_final_so_far() {
    let (data, cache) = dirs();
    let mut sam = String::from("@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:d\tLN:200000\n");
    let seq = "ACGT".repeat(25);
    for pos in 1..=70_000 {
        sam.push_str(&format!(
            "r{pos}\t0\td\t{pos}\t60\t100M\t*\t0\t0\t{seq}\t*\n"
        ));
    }
    let path = bam_from_sam(data.path(), "d.bam", &sam);
    let doc = super::BamDoc::open(&path, cache.path(), &crate::Progress::new()).unwrap();
    let mut seen = Vec::new();
    let whole = doc
        .source()
        .coverage(0, 4, &crate::Progress::new(), |c| seen.push(c.clone()))
        .unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].done_to, 65_535);
    assert_eq!(seen[0].covered, [(1, 65_535)]);
    assert_eq!(whole.covered, [(1, 70_099)]);
    assert_eq!(
        seen[0].next_start(
            200_000,
            10,
            super::Direction::Forward,
            super::Stretch::Empty
        ),
        None,
        "where coverage ends is not known yet"
    );
    assert_eq!(
        whole.next_start(
            200_000,
            10,
            super::Direction::Forward,
            super::Stretch::Empty
        ),
        Some(70_100)
    );
}

#[test]
fn a_reference_attaches_and_reads_stretches() {
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    open(&mut s, &f.bam).unwrap();
    let line = format!("bam ref {}", f.reference.display());
    let out = ex(&mut s, &line).unwrap();
    assert_eq!(out.message, "attached ref.fa to reads.bam (3 of 3 contigs)");
    let reference = s.datasets()[0].bam().unwrap().source().reference().unwrap();
    assert_eq!(reference.fetch("c1", 101, 110).unwrap(), b"ACGTACGTAC");
    assert!(reference.has("c3"));
    assert!(!data.path().join("ref.fa.fai").exists(), "built in memory");
    assert_eq!(s.history().last().unwrap(), &line);

    fs::write(data.path().join("short.fa"), ">c1\nACGT\n>c2\nTTGCA\n").unwrap();
    let out = ex(
        &mut s,
        &format!("bam ref {}", data.path().join("short.fa").display()),
    )
    .unwrap();
    assert_eq!(
        out.message,
        "attached short.fa to reads.bam (2 of 3 contigs; 1 is not in short.fa)"
    );

    fs::write(data.path().join("t.nwk"), "(A,B);").unwrap();
    open(&mut s, &data.path().join("t.nwk")).unwrap();
    assert_eq!(
        ex(&mut s, &line).unwrap_err().to_string(),
        "t.nwk is not a BAM; switch with use N"
    );
}

#[test]
fn gzip_references_need_bgzip_and_its_indexes() {
    use std::io::Write as _;
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    open(&mut s, &f.bam).unwrap();
    let text = fs::read(&f.reference).unwrap();

    let gz = data.path().join("ref.fa.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&text).unwrap();
    fs::write(&gz, encoder.finish().unwrap()).unwrap();
    assert_eq!(
        ex(&mut s, &format!("bam ref {}", gz.display()))
            .unwrap_err()
            .to_string(),
        "ref.fa.gz is gzip, not bgzip: reading a stretch needs bgzip -i"
    );

    let bgz = data.path().join("ref2.fa.gz");
    let mut writer = noodles::bgzf::io::Writer::new(File::create(&bgz).unwrap());
    writer.write_all(&text).unwrap();
    writer.finish().unwrap();
    assert_eq!(
        ex(&mut s, &format!("bam ref {}", bgz.display()))
            .unwrap_err()
            .to_string(),
        "ref2.fa.gz needs ref2.fa.gz.fai and ref2.fa.gz.gzi (samtools faidx makes both)"
    );
}

/// The names and flags of a BAM's records, and its header.
fn bam_records(path: &Path) -> (noodles::sam::Header, Vec<(String, u16)>) {
    let mut reader = bam::io::Reader::new(File::open(path).unwrap());
    let header = reader.read_header().unwrap();
    let records = reader
        .records()
        .map(|r| {
            let r = r.unwrap();
            (
                r.name().map(|n| n.to_string()).unwrap_or_default(),
                r.flags().bits(),
            )
        })
        .collect();
    (header, records)
}

#[test]
fn export_region_writes_every_overlapping_record_with_an_index() {
    let (data, cache) = dirs();
    let out = tempfile::tempdir().unwrap();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    open(&mut s, &f.bam).unwrap();

    let sub = out.path().join("sub.bam");
    let line = format!("bam export-region --out {} c1:100-160", sub.display());
    let done = ex(&mut s, &line).unwrap();
    assert_eq!(
        done.message,
        "wrote 2 records to sub.bam and its index sub.bam.bai"
    );
    let (header, records) = bam_records(&sub);
    assert_eq!(records, [("r1".to_string(), 99), ("r2".to_string(), 0)]);
    assert_eq!(header, bam_records(&f.bam).0);
    assert!(out.path().join("sub.bam.bai").exists());
    assert_eq!(s.history().last().unwrap(), &line);

    let u = out.path().join("u.bam");
    ex(
        &mut s,
        &format!("bam export-region c2:301-350 --out {}", u.display()),
    )
    .unwrap();
    assert_eq!(
        bam_records(&u).1,
        [("r7".to_string(), 73), ("r7".to_string(), 133)]
    );

    let cut = out.path().join("cut.bam");
    let done = ex(
        &mut s,
        &format!("bam export-region c1:901-5000 --out {}", cut.display()),
    )
    .unwrap();
    assert_eq!(
        done.message,
        "wrote 1 record to cut.bam and its index cut.bam.bai; \
         c1 ends at 1,000, so the region was cut to c1:901-1000"
    );

    let again = s.start(Command::parse_line(&line).unwrap());
    assert!(
        matches!(again, Err(Error::Exists(_))),
        "refused before the job starts"
    );
}

#[test]
fn export_ref_writes_the_stretch() {
    let (data, cache) = dirs();
    let out = tempfile::tempdir().unwrap();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    open(&mut s, &f.bam).unwrap();
    let fa = out.path().join("s.fa");
    let line = format!("bam export-ref c1:101-110 --out {}", fa.display());
    assert_eq!(
        ex(&mut s, &line).unwrap_err().to_string(),
        "export-ref needs the reference: bam ref FILE"
    );

    ex(&mut s, &format!("bam ref {}", f.reference.display())).unwrap();
    let done = ex(&mut s, &line).unwrap();
    assert_eq!(done.message, "wrote c1:101-110 (10 bp) to s.fa");
    assert_eq!(
        fs::read_to_string(&fa).unwrap(),
        ">c1:101-110\nACGTACGTAC\n"
    );

    let long = out.path().join("long.fa");
    ex(
        &mut s,
        &format!("bam export-ref c1:1-130 --out {}", long.display()),
    )
    .unwrap();
    let text = fs::read_to_string(&long).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], ">c1:1-130");
    assert_eq!(
        lines[1..].iter().map(|l| l.len()).collect::<Vec<_>>(),
        [60, 60, 10]
    );
}

#[test]
fn reads_export_once_each_in_their_own_orientation() {
    use super::testing::{ref_bases as r, revcomp};
    let (data, cache) = dirs();
    let out = tempfile::tempdir().unwrap();
    let f = fixture(data.path());
    let mut s = session(cache.path());
    open(&mut s, &f.bam).unwrap();
    let o = |name: &str| out.path().join(name);
    let read = |name: &str| fs::read_to_string(o(name)).unwrap();
    let mut r1 = r("c1", 101, 150).into_bytes();
    r1[9] = b'G';
    let r1 = String::from_utf8(r1).unwrap();
    let r2 = format!(
        "{}{}{}",
        r("c2", 51, 60),
        r("c1", 121, 150),
        r("c1", 156, 165)
    );

    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:101-150 --out {}",
            o("r.fa").display()
        ),
    )
    .unwrap();
    assert_eq!(done.message, "wrote 2 reads to r.fa");
    assert_eq!(read("r.fa"), format!(">r1/1\n{r1}\n>r2\n{r2}\n"));

    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:101-150 --with-mates --out {}",
            o("m.fa").display()
        ),
    )
    .unwrap();
    assert_eq!(done.message, "wrote 3 reads to m.fa");
    let r1_2 = revcomp(&r("c1", 301, 350));
    assert_eq!(
        read("m.fa"),
        format!(">r1/1\n{r1}\n>r2\n{r2}\n>r1/2\n{r1_2}\n")
    );

    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:101-150 --with-mates --out {}",
            o("m.bam").display()
        ),
    )
    .unwrap();
    assert_eq!(
        done.message,
        "wrote 4 records to m.bam and its index m.bam.bai"
    );
    let names: Vec<(String, u16)> = bam_records(&o("m.bam")).1;
    assert_eq!(
        names,
        [
            ("r1".to_string(), 99),
            ("r2".to_string(), 0),
            ("r1".to_string(), 147),
            ("r2".to_string(), 2048)
        ]
    );

    ex(
        &mut s,
        &format!(
            "bam export-reads --region c2:51-60 --out {}",
            o("s.fa").display()
        ),
    )
    .unwrap();
    assert_eq!(
        read("s.fa"),
        format!(">r2\n{r2}\n"),
        "the primary, found through SA"
    );

    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:701-760 --out {}",
            o("v.fq").display()
        ),
    )
    .unwrap();
    assert_eq!(
        done.message,
        "wrote 2 reads to v.fq; left out 1 secondary alignment"
    );
    let q = "!".repeat(40);
    assert_eq!(
        read("v.fq"),
        format!(
            "@r3\n{}\n+\n{q}\n@r5\n{}\n+\n{q}\n",
            r("c1", 701, 740),
            r("c1", 721, 760)
        )
    );

    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:101-150 --name r2 --name zz --out {}",
            o("n.fa").display()
        ),
    )
    .unwrap();
    assert_eq!(
        done.message,
        "wrote 1 read to n.fa; 1 name had no read in the region"
    );
    assert_eq!(read("n.fa"), format!(">r2\n{r2}\n"));

    ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:121-130 --clip --out {}",
            o("c.fa").display()
        ),
    )
    .unwrap();
    let mid = r("c1", 121, 130);
    assert_eq!(
        read("c.fa"),
        format!(">r1/1:121-130\n{mid}\n>r2:121-130\n{mid}\n")
    );

    ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:101-150 --out {}",
            o("r.fa.gz").display()
        ),
    )
    .unwrap();
    let mut text = String::new();
    use std::io::Read as _;
    flate2::read::GzDecoder::new(File::open(o("r.fa.gz")).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, read("r.fa"));

    let mut refused = |line: String| ex(&mut s, &line).unwrap_err().to_string();
    assert_eq!(
        refused(format!(
            "bam export-reads --region c1 --out {}",
            o("r.txt").display()
        )),
        "--out must end in .bam, .fa, .fasta, .fna, .fq or .fastq (or those with .gz)"
    );
    assert_eq!(
        refused(format!(
            "bam export-reads --region c1 --clip --out {}",
            o("c.bam").display()
        )),
        "--clip is for FASTA or FASTQ output"
    );
    assert_eq!(
        refused(format!(
            "bam export-reads --region c1 --clip --with-mates --out {}",
            o("cm.fa").display()
        )),
        "--clip keeps only the bases inside the region, so it cannot take --with-mates"
    );
    assert!(!o("r.txt").exists() && !o("c.bam").exists() && !o("cm.fa").exists());
}

#[test]
fn a_names_file_sits_beside_the_log() {
    let (data, cache) = dirs();
    let logs = tempfile::tempdir().unwrap();
    let f = fixture(data.path());
    let log = logs.path().join("seqpeeq-20261117-120000.log");
    let mut s = Session::new(Some(crate::SessionLog::new(log)));
    s.set_cache_dir(cache.path().to_path_buf());
    open(&mut s, &f.bam).unwrap();

    let first = s.names_file(&["r2".to_string()]).unwrap().unwrap();
    assert_eq!(
        first,
        logs.path().join("seqpeeq-20261117-120000.names-1.txt")
    );
    assert_eq!(fs::read_to_string(&first).unwrap(), "r2\n");
    let second = s
        .names_file(&["a".to_string(), "b".to_string()])
        .unwrap()
        .unwrap();
    assert_eq!(
        second,
        logs.path().join("seqpeeq-20261117-120000.names-2.txt")
    );
    assert_eq!(fs::read_to_string(&second).unwrap(), "a\nb\n");
    assert_eq!(
        session(cache.path())
            .names_file(&["r2".to_string()])
            .unwrap(),
        None
    );

    let x = data.path().join("x.fa");
    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c1:101-150 --names-file {} --out {}",
            first.display(),
            x.display()
        ),
    )
    .unwrap();
    assert_eq!(done.message, "wrote 1 read to x.fa");
    assert!(fs::read_to_string(&x).unwrap().starts_with(">r2\n"));

    let empty = data.path().join("empty.txt");
    fs::write(&empty, "\n").unwrap();
    let line = format!(
        "bam export-reads --region c1 --names-file {} --out {}",
        empty.display(),
        data.path().join("e.fa").display()
    );
    assert_eq!(
        ex(&mut s, &line).unwrap_err().to_string(),
        "empty.txt names no reads"
    );
    let missing = data.path().join("none.txt");
    let line = format!(
        "bam export-reads --region c1 --names-file {} --out {}",
        missing.display(),
        data.path().join("e.fa").display()
    );
    assert!(matches!(ex(&mut s, &line), Err(Error::Io { .. })));
}

#[test]
fn clipping_keeps_insertions_and_counts_what_could_not_be_written() {
    let (data, cache) = dirs();
    let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:1000\n\
        a\t0\tc\t1\t60\t5M2I5M\t*\t0\t0\tACGTAGGCGTAC\t*\n\
        b\t0\tc\t1\t60\t2M20D2M\t*\t0\t0\tACGT\t*\n\
        s\t2048\tc\t100\t60\t10M\t*\t0\t0\tACGTACGTAC\t*\tSA:Z:c,500,+,10M,60,0;\n\
        p\t97\tc\t200\t60\t10M\tc\t600\t0\tACGTACGTAC\t*\n";
    let bam = bam_from_sam(data.path(), "x.bam", sam);
    let mut s = session(cache.path());
    open(&mut s, &bam).unwrap();
    let c = data.path().join("c.fa");
    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c:3-8 --clip --out {}",
            c.display()
        ),
    )
    .unwrap();
    assert_eq!(
        done.message,
        "wrote 1 read to c.fa; 1 read had no base in the region"
    );
    assert_eq!(fs::read_to_string(&c).unwrap(), ">a:3-8\nGTAGGCGT\n");

    let m = data.path().join("m.fa");
    let done = ex(
        &mut s,
        &format!(
            "bam export-reads --region c:100-210 --with-mates --out {}",
            m.display()
        ),
    )
    .unwrap();
    assert_eq!(
        done.message,
        "wrote 2 reads to m.fa; 1 read written from a split part (no primary found); \
         2 mates or parts not found"
    );
    assert_eq!(
        fs::read_to_string(&m).unwrap(),
        ">s\nACGTACGTAC\n>p/1\nACGTACGTAC\n"
    );
}

#[test]
fn a_bam_session_replays_to_the_same_files() {
    let (data, cache) = dirs();
    let logs = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let f = fixture(data.path());
    let log = logs.path().join("s.log");
    let mut s = Session::new(Some(crate::SessionLog::new(log.clone())));
    s.set_cache_dir(cache.path().to_path_buf());
    open(&mut s, &f.bam).unwrap();
    let names = s
        .names_file(&["r2".to_string(), "r6".to_string()])
        .unwrap()
        .unwrap();
    let to = |name: &str| a.path().join(name).display().to_string();
    for line in [
        format!("bam ref {}", f.reference.display()),
        format!("bam export-region --out {} c1:100-960", to("sub.bam")),
        format!("bam export-ref --out {} c2", to("c2.fa")),
        format!(
            "bam export-reads --region c1:101-150 --with-mates --out {}",
            to("mates.fa.gz")
        ),
        format!(
            "bam export-reads --region c1 --names-file {} --out {}",
            names.display(),
            to("named.bam")
        ),
    ] {
        ex(&mut s, &line).unwrap();
    }
    drop(s);

    let text = fs::read_to_string(&log).unwrap().replace(
        &a.path().display().to_string(),
        &b.path().display().to_string(),
    );
    let mut again = session(cache.path());
    let report = crate::replay(&text, &mut again).unwrap();
    assert_eq!(report.commands, 6);

    let mut written: Vec<_> = fs::read_dir(a.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    written.sort();
    assert_eq!(
        written,
        [
            "c2.fa",
            "mates.fa.gz",
            "named.bam",
            "named.bam.bai",
            "sub.bam",
            "sub.bam.bai"
        ]
    );
    for name in written {
        assert_eq!(
            fs::read(a.path().join(&name)).unwrap(),
            fs::read(b.path().join(&name)).unwrap(),
            "{name:?} differs"
        );
    }
}

// ---- Final review fixes ----

const MANY: usize = 30_000;

/// A BAM over `MANY` contigs named `k0`… (100 bp each), with `rows` as its
/// records, and a FASTA of all of them.
fn many_contigs(dir: &Path, rows: &[String]) -> (PathBuf, PathBuf) {
    let mut sam = String::from("@HD\tVN:1.6\tSO:coordinate\n");
    let mut fa = String::new();
    for i in 0..MANY {
        sam.push_str(&format!("@SQ\tSN:k{i}\tLN:100\n"));
        fa.push_str(&format!(">k{i}\n{}\n", "ACGT".repeat(25)));
    }
    for row in rows {
        sam.push_str(row);
        sam.push('\n');
    }
    let reference = dir.join("many.fa");
    fs::write(&reference, fa).unwrap();
    (bam_from_sam(dir, "many.bam", &sam), reference)
}

fn timed<T>(limit: f64, what: &str, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let out = f();
    let took = start.elapsed().as_secs_f64();
    assert!(took < limit, "{what} took {took:.2} s (limit {limit} s)");
    out
}

#[test]
fn a_reference_attaches_quickly_to_many_contigs() {
    let (data, cache) = dirs();
    let (bam, reference) = many_contigs(data.path(), &[]);
    let mut s = session(cache.path());
    open(&mut s, &bam).unwrap();
    let line = format!("bam ref {}", reference.display());
    let done = timed(3.0, "bam ref", || ex(&mut s, &line).unwrap());
    assert_eq!(
        done.message,
        "attached many.fa to many.bam (30,000 of 30,000 contigs)"
    );
}

#[test]
fn mates_and_parts_on_other_contigs_are_found_quickly() {
    let (data, cache) = dirs();
    let seq = "ACGT".repeat(5);
    let mut rows = Vec::new();
    for i in 1..=300 {
        rows.push(format!(
            "m{i}\t65\tk0\t1\t60\t20M\tk{}\t1\t0\t{seq}\t*\tSA:Z:k{},50,+,20M,60,0;",
            MANY - i,
            MANY - i
        ));
    }
    for i in (1..=300).rev() {
        let c = MANY - i;
        rows.push(format!("m{i}\t129\tk{c}\t1\t60\t20M\tk0\t1\t0\t{seq}\t*"));
        rows.push(format!("m{i}\t2113\tk{c}\t50\t60\t20M\tk0\t1\t0\t{seq}\t*"));
    }
    let (bam, _) = many_contigs(data.path(), &rows);
    let mut s = session(cache.path());
    open(&mut s, &bam).unwrap();
    let out = data.path().join("m.bam");
    let line = format!(
        "bam export-reads --region k0 --with-mates --out {}",
        out.display()
    );
    let done = timed(5.0, "export-reads --with-mates", || {
        ex(&mut s, &line).unwrap()
    });
    assert_eq!(
        done.message,
        "wrote 900 records to m.bam and its index m.bam.bai"
    );
}

#[test]
fn split_parts_resolve_quickly_with_many_contigs() {
    let (data, cache) = dirs();
    let seq = "ACGT".repeat(5);
    let rows: Vec<String> = (0..3_000)
        .map(|i| {
            format!(
                "s{i}\t0\tk0\t1\t60\t20M\t*\t0\t0\t{seq}\t*\tSA:Z:k{},1,+,20M,60,0;",
                MANY - 1
            )
        })
        .collect();
    let (bam, _) = many_contigs(data.path(), &rows);
    let doc = super::BamDoc::open(&bam, cache.path(), &crate::Progress::new()).unwrap();
    let source = doc.source();
    let all = region(source, "k0");
    let reads = timed(1.0, "reads", || {
        source.reads(&all, &crate::Progress::new()).unwrap()
    });
    assert_eq!(reads.len(), 3_000);
    assert_eq!(reads[0].parts[0].place.contig, MANY - 1);
}

#[test]
fn with_mates_stays_quick_when_mates_are_in_the_region() {
    let (data, cache) = dirs();
    let seq = "ACGT".repeat(5);
    let mut sam = String::from("@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:w\tLN:100000\n");
    for i in 0..20_000u64 {
        let p = 1 + i * 2;
        sam.push_str(&format!(
            "q{i}\t65\tw\t{p}\t60\t20M\t=\t{}\t0\t{seq}\t*\n",
            p + 1
        ));
        sam.push_str(&format!(
            "q{i}\t129\tw\t{}\t60\t20M\t=\t{p}\t0\t{seq}\t*\n",
            p + 1
        ));
    }
    let bam = bam_from_sam(data.path(), "w.bam", &sam);
    let mut s = session(cache.path());
    open(&mut s, &bam).unwrap();
    let out = data.path().join("w.fa");
    let line = format!(
        "bam export-reads --region w --with-mates --out {}",
        out.display()
    );
    let done = timed(5.0, "export-reads --with-mates", || {
        ex(&mut s, &line).unwrap()
    });
    assert_eq!(done.message, "wrote 40000 reads to w.fa");
}

#[test]
fn a_bgzipped_reference_reads_whatever_its_name() {
    use std::io::Write as _;
    let (data, cache) = dirs();
    let f = fixture(data.path());
    let text = fs::read(&f.reference).unwrap();
    // One BGZF block, so the .gzi lists no further blocks.
    let fai = noodles::fasta::fs::index(&f.reference).unwrap();
    for name in ["ref.fa.gz", "ref.fa.bgzf"] {
        let path = data.path().join(name);
        let mut writer = noodles::bgzf::io::Writer::new(File::create(&path).unwrap());
        writer.write_all(&text).unwrap();
        writer.finish().unwrap();
        let mut fai_out = noodles::fasta::fai::io::Writer::new(
            File::create(data.path().join(format!("{name}.fai"))).unwrap(),
        );
        fai_out.write_index(&fai).unwrap();
        fs::write(data.path().join(format!("{name}.gzi")), 0u64.to_le_bytes()).unwrap();
    }
    let mut s = session(cache.path());
    open(&mut s, &f.bam).unwrap();
    for name in ["ref.fa.gz", "ref.fa.bgzf"] {
        ex(
            &mut s,
            &format!("bam ref {}", data.path().join(name).display()),
        )
        .unwrap();
        let reference = s.datasets()[0].bam().unwrap().source().reference().unwrap();
        assert_eq!(
            reference.fetch("c2", 1, 12).unwrap(),
            b"TTGCATTGCATT",
            "{name}"
        );
        assert_eq!(
            reference.fetch("c1", 999, 1200).unwrap(),
            b"GT",
            "{name}: cut to the end"
        );
    }
}

#[test]
fn a_jump_from_position_0_does_not_panic() {
    use super::{Direction, Stretch};
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    assert_eq!(
        source
            .jump(0, 0, Direction::Forward, Stretch::Covered, &progress)
            .unwrap(),
        Some(101)
    );
    assert_eq!(
        source
            .jump(0, 0, Direction::Back, Stretch::Covered, &progress)
            .unwrap(),
        None
    );
    assert_eq!(
        source
            .jump(2, 0, Direction::Forward, Stretch::Empty, &progress)
            .unwrap(),
        Some(1)
    );
}

#[test]
fn a_deletion_does_not_end_a_covered_stretch() {
    // Long reads carry a small deletion every few dozen bases; a jump to the
    // next empty stretch must not stop inside a read. An N (intron) still
    // splits.
    use super::{Direction, Stretch};
    let (data, cache) = dirs();
    let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:1000\n\
        d\t0\tc\t1\t60\t10M3D10M\t*\t0\t0\tACGTACGTACACGTACGTAC\t*\n\
        n\t0\tc\t101\t60\t10M50N10M\t*\t0\t0\tACGTACGTACACGTACGTAC\t*\n";
    let bam = bam_from_sam(data.path(), "d.bam", sam);
    let doc = super::BamDoc::open(&bam, cache.path(), &crate::Progress::new()).unwrap();
    let progress = crate::Progress::new();
    let pass = doc.source().coverage(0, 10, &progress, |_| {}).unwrap();
    assert_eq!(pass.covered, [(1, 23), (101, 110), (161, 170)]);
    assert_eq!(
        doc.source()
            .jump(0, 1, Direction::Forward, Stretch::Empty, &progress)
            .unwrap(),
        Some(24)
    );
    // Depth still counts only aligned bases: bin 1 (1-100) holds 20 bases.
    assert_eq!(pass.bins[0], 0.2);
}

#[test]
fn a_moved_log_finds_its_names_file_beside_it() {
    let (data, cache) = dirs();
    let first = tempfile::tempdir().unwrap();
    let moved = tempfile::tempdir().unwrap();
    let f = fixture(data.path());
    let log = first.path().join("s.log");
    let mut s = Session::new(Some(crate::SessionLog::new(log.clone())));
    s.set_cache_dir(cache.path().to_path_buf());
    open(&mut s, &f.bam).unwrap();
    let names = s.names_file(&["r2".to_string()]).unwrap().unwrap();
    let a = data.path().join("a.fa");
    let line = format!(
        "bam export-reads --region c1 --names-file {} --out {}",
        names.display(),
        a.display()
    );
    ex(&mut s, &line).unwrap();
    drop(s);

    // The log and its names file move together; the old folder is gone.
    let text = fs::read_to_string(&log).unwrap().replace("a.fa", "b.fa");
    fs::write(moved.path().join("s.log"), &text).unwrap();
    fs::rename(&names, moved.path().join(names.file_name().unwrap())).unwrap();
    drop(first);

    let mut again = session(cache.path());
    again.set_replay_dir(Some(moved.path().to_path_buf()));
    crate::replay(&text, &mut again).unwrap();
    assert_eq!(
        fs::read(&a).unwrap(),
        fs::read(data.path().join("b.fa")).unwrap()
    );
}

#[test]
fn a_window_gives_binned_depth_and_its_reads() {
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    let w = source
        .window(&region(&source, "c1:101-200"), 10, Some(100), &progress)
        .unwrap();
    assert_eq!(
        w.depth,
        [1.0, 1.0, 2.0, 2.0, 2.0, 0.5, 0.5, 0.0, 0.0, 0.0],
        "deleted bases are not depth"
    );
    let names: Vec<String> = w.reads.unwrap().iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["r1", "r2"]);
    assert_eq!((w.bin, w.count), (10, 2));
    assert_eq!(progress.fraction(), Some(1.0));

    // c1 has seven mapped records: r1 twice, r2, r3, r4 (secondary), r5
    // (duplicate) and r6.
    let all = source
        .window(
            &region(&source, "c1"),
            100,
            Some(2),
            &crate::Progress::new(),
        )
        .unwrap();
    assert_eq!((all.reads.is_none(), all.count), (true, 7));
    let depth_only = source
        .window(&region(&source, "c1"), 100, None, &crate::Progress::new())
        .unwrap();
    assert!(depth_only.reads.is_none());
    assert_eq!(depth_only.depth.len(), 10);
    assert_eq!(
        depth_only.depth[1], 0.9,
        "101-200: r1's 50 and r2's 40 bases"
    );
    // r4 and r5 do not count towards depth.
    assert_eq!(depth_only.depth[7], 0.4, "701-800: r3's 40 bases");

    // A last bin cut short by the region's end is averaged over what it has.
    let short = source
        .window(
            &region(&source, "c1:141-155"),
            10,
            None,
            &crate::Progress::new(),
        )
        .unwrap();
    assert_eq!(short.depth, [2.0, 0.0]);
}

#[test]
fn a_cancelled_window_stops() {
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    progress.cancel();
    let err = source
        .window(&region(&source, "c1"), 1, Some(10), &progress)
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

#[test]
fn a_jump_reports_progress() {
    use super::{Direction, Stretch};
    let (data, cache) = dirs();
    let source = fixture_source(data.path(), cache.path());
    let progress = crate::Progress::new();
    source
        .jump(0, 1, Direction::Forward, Stretch::Empty, &progress)
        .unwrap();
    assert!(progress.fraction().is_some(), "a forward jump");
    let progress = crate::Progress::new();
    source
        .jump(0, 1000, Direction::Back, Stretch::Covered, &progress)
        .unwrap();
    assert!(progress.fraction().is_some(), "a jump back");
}

fn multi_source(data: &Path, cache: &Path) -> super::BamSource {
    let bam = super::testing::multi(data);
    super::BamDoc::open(&bam, cache, &crate::Progress::new())
        .unwrap()
        .source()
        .clone()
}

/// The mapped reads of `source` overlapping `text`, by name and start.
fn read_at(source: &super::BamSource, text: &str, name: &str, start: u64) -> super::Read {
    let region = super::parse_region(text, source.contigs(), "multi.bam")
        .unwrap()
        .0;
    source
        .reads(&region, &crate::Progress::new())
        .unwrap()
        .into_iter()
        .find(|r| r.name == name && r.start == start)
        .unwrap()
}

#[test]
fn read_coordinates_flip_for_the_reverse_strand_and_count_clips() {
    use super::{parse_cigar, read_span, segment};
    let c = parse_cigar("10S30M5D10M").unwrap();
    assert_eq!(read_span(&c, false), Some((11, 50, 50)));
    assert_eq!(read_span(&c, true), Some((1, 40, 50)));
    let h = parse_cigar("60H40M").unwrap();
    assert_eq!(read_span(&h, false), Some((61, 100, 100)));
    assert_eq!(read_span(&h, true), Some((1, 40, 100)));
    assert_eq!(
        read_span(&parse_cigar("5M2I3M").unwrap(), false),
        Some((1, 10, 10))
    );
    assert_eq!(parse_cigar("*"), None);
    assert_eq!(parse_cigar("10Q"), None);
    assert_eq!(parse_cigar(""), None);
    assert_eq!(read_span(&parse_cigar("20S").unwrap(), false), None);
    assert_eq!(
        (segment(99), segment(147), segment(0), segment(2048)),
        (1, 2, 0, 0)
    );
}

#[test]
fn a_read_carries_its_tags_and_alternatives() {
    use super::{Alt, Place};
    let (data, cache) = dirs();
    let source = multi_source(data.path(), cache.path());
    let x1 = read_at(&source, "g1:101-101", "x1", 101);
    assert_eq!(
        (x1.nm, x1.score, x1.suboptimal),
        (Some(0), Some(50), Some(50))
    );
    assert_eq!(
        x1.alts,
        [
            Alt {
                place: Place {
                    contig: 1,
                    pos: 301,
                    reverse: false
                },
                cigar: "50M".to_string(),
                nm: Some(1),
            },
            Alt {
                place: Place {
                    contig: 2,
                    pos: 501,
                    reverse: true
                },
                cigar: "50M".to_string(),
                nm: Some(2),
            },
        ]
    );
    assert_eq!(x1.alts_skipped, 0);
    let z1 = read_at(&source, "g1:1001-1001", "z1", 1001);
    assert_eq!(z1.parts[0].nm, Some(1));
    let y1 = read_at(&source, "g1:201-201", "y1", 201);
    assert!(y1.alts.is_empty());
    assert_eq!((y1.nm, y1.score, y1.suboptimal), (None, None, None));
}

#[test]
fn unreadable_xa_entries_are_skipped_and_counted() {
    let (data, cache) = dirs();
    let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:g1\tLN:2000\n@SQ\tSN:g2\tLN:2000\n\
               q\t0\tg1\t1\t0\t50M\t*\t0\t0\t*\t*\tXA:Z:g2,+301,50M,1;bad;zz,+1,50M,0;g2,301,50M,1;g2,+5,5Q,0;\n";
    let bam = bam_from_sam(data.path(), "q.bam", sam);
    let source = super::BamDoc::open(&bam, cache.path(), &crate::Progress::new())
        .unwrap()
        .source()
        .clone();
    let q = read_at(&source, "g1:1-1", "q", 1);
    assert_eq!(q.alts.len(), 1);
    assert_eq!(q.alts_skipped, 4);
}

fn at(reads: &[super::Read]) -> Vec<(u16, usize, u64)> {
    reads.iter().map(|r| (r.flags, r.contig, r.start)).collect()
}

#[test]
fn gather_follows_the_mate_and_split_parts_through_the_index() {
    let (data, cache) = dirs();
    let source = multi_source(data.path(), cache.path());
    let p = crate::Progress::new();
    let x1 = read_at(&source, "g1:101-101", "x1", 101);
    assert_eq!(at(&source.gather(&x1, &p).unwrap()), [(147, 0, 401)]);
    let z1 = read_at(&source, "g1:1001-1001", "z1", 1001);
    assert_eq!(at(&source.gather(&z1, &p).unwrap()), [(2048, 1, 1501)]);
    let z1s = read_at(&source, "g2:1501-1501", "z1", 1501);
    assert_eq!(at(&source.gather(&z1s, &p).unwrap()), [(0, 0, 1001)]);
    let p1 = read_at(&source, "g1:1201-1201", "p1", 1201);
    assert_eq!(at(&source.gather(&p1, &p).unwrap()), [(129, 2, 801)]);
    let y1 = read_at(&source, "g1:201-201", "y1", 201);
    assert!(source.gather(&y1, &p).unwrap().is_empty());
}

#[test]
fn a_name_search_reads_the_whole_file_unmapped_records_too() {
    let (data, cache) = dirs();
    let source = multi_source(data.path(), cache.path());
    let p = crate::Progress::new();
    let names: std::collections::HashSet<String> =
        ["y1", "u1", "nope"].iter().map(|s| s.to_string()).collect();
    let found = source.search_names(&names, &p).unwrap();
    assert_eq!(
        at(&found),
        [
            (0, 0, 201),
            (256, 1, 801),
            (256, 2, 101),
            (73, 2, 901),
            (133, 2, 901)
        ]
    );
    assert!(found[4].flags & 0x4 != 0, "the unmapped mate");
    assert_eq!(p.fraction(), Some(1.0));
}

#[test]
fn a_cancelled_search_stops() {
    let (data, cache) = dirs();
    let source = multi_source(data.path(), cache.path());
    let p = crate::Progress::new();
    p.cancel();
    let names = std::collections::HashSet::from(["y1".to_string()]);
    assert!(matches!(
        source.search_names(&names, &p),
        Err(Error::Cancelled)
    ));
}
