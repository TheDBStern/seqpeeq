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
            mapq: 60
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
