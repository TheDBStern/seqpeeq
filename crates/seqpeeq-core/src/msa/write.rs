//! Writing an alignment, or part of one, to a new FASTA file.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

use super::Alignment;
use crate::ranges::Ranges;
use crate::{Error, Result};

/// Write `rows` (and only `cols` of them, if given) to a new file at `out`,
/// one line per sequence or in lines of `wrap` residues. Returns the size of
/// the finished file in bytes.
///
/// An existing file is an error, never replaced. A name ending in `.gz` is
/// written gzip-compressed.
pub(crate) fn write_fasta(
    aln: &Alignment,
    rows: &[usize],
    cols: Option<&Ranges>,
    wrap: Option<usize>,
    out: &Path,
) -> Result<u64> {
    let gzip = out
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));
    create_then(out, |file| write_records(aln, rows, cols, wrap, file, gzip))
}

/// Create `out` as a new file with the contents `fill` writes. Returns its
/// size in bytes.
///
/// The contents go to a partial file beside `out`, which is given the final
/// name only once complete. So a run that fails or is killed part-way never
/// leaves a short file that looks finished, and a file that already has the
/// name, or takes it meanwhile, is never replaced.
fn create_then(out: &Path, fill: impl FnOnce(File) -> io::Result<()>) -> Result<u64> {
    let failed = |cause| Error::Io {
        path: out.to_path_buf(),
        cause,
    };
    let taken = || out.symlink_metadata().is_ok();
    if taken() {
        return Err(Error::Exists(out.to_path_buf()));
    }
    let (partial, file) = create_partial(out).map_err(failed)?;
    let published = fill(file).map_err(failed).and_then(|()| {
        // A hard link fails if the name exists, which makes this step safe
        // against a file created while we were writing.
        match std::fs::hard_link(&partial, out) {
            Ok(()) => Ok(()),
            Err(cause) if cause.kind() == io::ErrorKind::AlreadyExists => {
                Err(Error::Exists(out.to_path_buf()))
            }
            // Some file systems have no hard links; there, check and rename.
            Err(_) if taken() => Err(Error::Exists(out.to_path_buf())),
            Err(_) => std::fs::rename(&partial, out).map_err(failed),
        }
    });
    // After a rename the partial name is already gone; that is fine.
    let _ = std::fs::remove_file(&partial);
    published?;
    Ok(std::fs::metadata(out).map_err(failed)?.len())
}

/// Create a new file named after `out` with `.partial-<process id>` added,
/// trying further names if one is left over from an earlier run.
fn create_partial(out: &Path) -> io::Result<(PathBuf, File)> {
    let name = out
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file name"))?;
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for attempt in 0..100 {
        let mut partial = name.to_os_string();
        partial.push(format!(".partial-{}", std::process::id()));
        if attempt > 0 {
            partial.push(format!("-{attempt}"));
        }
        let path = out.with_file_name(partial);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

fn write_records(
    aln: &Alignment,
    rows: &[usize],
    cols: Option<&Ranges>,
    wrap: Option<usize>,
    file: File,
    gzip: bool,
) -> io::Result<()> {
    let mut buffered = BufWriter::with_capacity(1 << 20, file);
    // Each writer is finished by hand: dropping one would hide a failure to
    // write its last bytes.
    if gzip {
        // The default header holds no file name and no time, so the same
        // content always compresses to the same bytes.
        let mut encoder = GzEncoder::new(buffered, Compression::default());
        records(aln, rows, cols, wrap, &mut encoder)?;
        encoder.finish()?.flush()
    } else {
        records(aln, rows, cols, wrap, &mut buffered)?;
        buffered.flush()
    }
}

fn records(
    aln: &Alignment,
    rows: &[usize],
    cols: Option<&Ranges>,
    wrap: Option<usize>,
    out: &mut impl Write,
) -> io::Result<()> {
    let mut residues = Vec::new();
    for &row in rows {
        out.write_all(b">")?;
        out.write_all(aln.name(row).as_bytes())?;
        out.write_all(b"\n")?;
        residues.clear();
        match cols {
            Some(cols) => {
                for range in cols.as_slice() {
                    aln.residues(row, range.clone(), &mut residues);
                }
            }
            None => aln.residues(row, 0..aln.row_len(row), &mut residues),
        }
        match wrap {
            Some(width) => {
                for line in residues.chunks(width) {
                    out.write_all(line)?;
                    out.write_all(b"\n")?;
                }
            }
            None => {
                out.write_all(&residues)?;
                out.write_all(b"\n")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::create_then;
    use crate::msa::testing::{TRIO, grid, run};
    use crate::msa::{Alignment, Applied};
    use crate::{Error, Result};
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};

    /// Apply `msa extract <flags> --out <path>`.
    fn extract(aln: &mut Alignment, flags: &str, out: &Path) -> Result<Applied> {
        let out = shlex::try_quote(out.to_str().unwrap()).unwrap();
        run(aln, &format!("extract {flags} --out {out}"))
    }

    fn dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn files(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn whole_alignment_one_line_per_sequence() {
        let (_keep, dir) = dir();
        let out = dir.join("x.fa");
        let done = extract(&mut Alignment::from_text(TRIO), "", &out).unwrap();
        assert_eq!(read(&out), TRIO);
        assert_eq!(
            done.message,
            format!("wrote {} (3 sequences x 10 columns)", out.display())
        );
        let note = done.note.unwrap();
        assert!(
            note.starts_with("output /") && note.ends_with("x.fa size=55"),
            "{note}"
        );
        assert_eq!(done.created, Some(out));
    }

    #[test]
    fn rows_columns_and_wrapping() {
        let (_keep, dir) = dir();
        let mut a = Alignment::from_text(TRIO);
        let done = extract(&mut a, "--rows 1,3 --cols 2-4,8", &dir.join("x.fa")).unwrap();
        assert_eq!(read(&dir.join("x.fa")), ">s1\nCGTG\n>s3 third one\nC--G\n");
        assert!(
            done.message.ends_with("(2 sequences x 4 columns)"),
            "{}",
            done.message
        );
        extract(&mut a, "--match s --cols 1-10 --wrap 4", &dir.join("y.fa")).unwrap();
        assert!(read(&dir.join("y.fa")).starts_with(">s1\nACGT\n-ACG\nTA\n>s2\nACGT\nTACG\n-A\n"));
        extract(&mut a, "--rows 1 --wrap 5", &dir.join("z.fa")).unwrap();
        assert_eq!(read(&dir.join("z.fa")), ">s1\nACGT-\nACGTA\n");
    }

    #[test]
    fn output_reflects_edits_and_loads_back_identically() {
        let (_keep, dir) = dir();
        let out = dir.join("x.fa");
        let mut a = Alignment::from_text(TRIO);
        run(&mut a, "delete-cols --cols 1-3").unwrap();
        run(&mut a, "fill --rows 1 --cols 1-2 --with N").unwrap();
        run(&mut a, "rename --rows 2 --to two").unwrap();
        extract(&mut a, "", &out).unwrap();
        assert_eq!(
            read(&out),
            ">s1\nNNACGTA\n>two\nTTACG-A\n>s3 third one\n-TACGTA\n"
        );
        assert_eq!(grid(&Alignment::load(&out).unwrap()), grid(&a));
    }

    #[test]
    fn bytes_are_preserved() {
        let (_keep, dir) = dir();
        let text = ">a\nacgtNRYn.-*X\n";
        extract(&mut Alignment::from_text(text), "", &dir.join("x.fa")).unwrap();
        assert_eq!(read(&dir.join("x.fa")), text);
    }

    #[test]
    fn gz_output() {
        let (_keep, dir) = dir();
        let out = dir.join("x.FA.GZ");
        extract(&mut Alignment::from_text(TRIO), "", &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(bytes[..2], [0x1f, 0x8b]);
        let mut plain = String::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_string(&mut plain)
            .unwrap();
        assert_eq!(plain, TRIO);
        assert_eq!(
            grid(&Alignment::load(&out).unwrap()),
            grid(&Alignment::from_text(TRIO))
        );
        // The same content always gives the same bytes.
        extract(
            &mut Alignment::from_text(TRIO),
            "",
            &dir.join("again.fa.gz"),
        )
        .unwrap();
        assert_eq!(std::fs::read(dir.join("again.fa.gz")).unwrap(), bytes);
    }

    #[test]
    fn existing_files_are_never_replaced() {
        let (_keep, dir) = dir();
        let taken = dir.join("x.fa");
        std::fs::write(&taken, "keep me").unwrap();
        let error = extract(&mut Alignment::from_text(TRIO), "", &taken).unwrap_err();
        assert!(matches!(error, Error::Exists(_)));
        assert!(
            error
                .to_string()
                .ends_with("x.fa already exists; choose another name or remove it first"),
            "{error}"
        );
        assert_eq!(read(&taken), "keep me");

        let input = dir.join("in.fa");
        std::fs::write(&input, TRIO).unwrap();
        let mut a = Alignment::load(&input).unwrap();
        run(&mut a, "delete-cols --cols 1").unwrap();
        assert!(matches!(extract(&mut a, "", &input), Err(Error::Exists(_))));
        assert_eq!(read(&input), TRIO);
    }

    #[test]
    fn failures_leave_nothing_behind() {
        let (_keep, dir) = dir();
        let mut a = Alignment::from_text(TRIO);
        assert!(matches!(
            extract(&mut a, "", &dir.join("missing").join("x.fa")),
            Err(Error::Io { .. })
        ));
        for (flags, message) in [
            ("--wrap 0", "--wrap must be at least 1"),
            ("--rows 9", "row 9 is past the last row (3)"),
            ("--cols 11", "column 11 is past the last column (10)"),
            ("--match zz", "no sequence name matches 'zz'"),
        ] {
            assert_eq!(
                extract(&mut a, flags, &dir.join("x.fa"))
                    .unwrap_err()
                    .to_string(),
                message
            );
        }
        let mut ragged = Alignment::from_text(">a\nACGT\n>b\nAC\n");
        assert_eq!(
            extract(&mut ragged, "--cols 1", &dir.join("x.fa"))
                .unwrap_err()
                .to_string(),
            "sequences differ in length, so columns cannot be selected"
        );
        assert_eq!(files(&dir), 0);
        let done = extract(&mut ragged, "--wrap 3", &dir.join("r.fa")).unwrap();
        assert!(done.message.ends_with("(2 sequences)"), "{}", done.message);
        assert_eq!(read(&dir.join("r.fa")), ">a\nACG\nT\n>b\nAC\n");
    }

    #[test]
    fn extract_does_not_disturb_undo() {
        let (_keep, dir) = dir();
        let mut a = Alignment::from_text(TRIO);
        run(&mut a, "delete-cols --cols 1").unwrap();
        a.undo().unwrap();
        extract(&mut a, "", &dir.join("x.fa")).unwrap();
        a.redo().unwrap();
        extract(&mut a, "", &dir.join("y.fa")).unwrap();
        a.undo().unwrap();
        assert_eq!(a.cols(), 10);
    }

    #[test]
    fn the_final_name_appears_only_when_the_file_is_complete() {
        let (_keep, dir) = dir();
        let out = dir.join("x.fa");
        let size = create_then(&out, |mut file| {
            assert!(!out.exists(), "the final name must not exist while writing");
            assert_eq!(files(&dir), 1, "the partial file");
            file.write_all(b"done\n")
        })
        .unwrap();
        assert_eq!((size, read(&out).as_str(), files(&dir)), (5, "done\n", 1));
    }

    #[test]
    fn a_failed_write_leaves_no_file_of_any_name() {
        let (_keep, dir) = dir();
        let result = create_then(&dir.join("x.fa"), |mut file| {
            file.write_all(b"half")?;
            Err(io::Error::other("disk full"))
        });
        assert!(matches!(result, Err(Error::Io { .. })));
        assert_eq!(files(&dir), 0);
    }

    #[test]
    fn a_name_taken_while_writing_is_not_replaced() {
        let (_keep, dir) = dir();
        let out = dir.join("x.fa");
        let result = create_then(&out, |mut file| {
            std::fs::write(&out, "theirs")?;
            file.write_all(b"mine")
        });
        assert!(matches!(result, Err(Error::Exists(_))));
        assert_eq!((read(&out).as_str(), files(&dir)), ("theirs", 1));
    }

    #[test]
    fn a_partial_file_left_by_a_killed_run_does_not_block_a_new_one() {
        let (_keep, dir) = dir();
        let stale = dir.join(format!("x.fa.partial-{}", std::process::id()));
        std::fs::write(&stale, "old").unwrap();
        extract(&mut Alignment::from_text(TRIO), "", &dir.join("x.fa")).unwrap();
        assert_eq!(read(&dir.join("x.fa")), TRIO);
        assert_eq!(read(&stale), "old");
    }
}
