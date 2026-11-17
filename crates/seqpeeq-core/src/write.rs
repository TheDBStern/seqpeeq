//! Creating output files: only ever new ones, and never a short one under
//! the final name.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::{Error, Result};

/// Create `out` as a new file holding what `body` writes, gzip-compressed
/// when the name ends in `.gz`. Returns its size in bytes. See
/// [`create_then`].
pub(crate) fn create_text(
    out: &Path,
    body: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<u64> {
    let gzip = out
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));
    create_then(out, |file| {
        let mut buffered = BufWriter::with_capacity(1 << 20, file);
        // Each writer is finished by hand: dropping one would hide a failure
        // to write its last bytes.
        if gzip {
            // The default header holds no file name and no time, so the same
            // content always compresses to the same bytes.
            // Writers hand over a name or a bracket at a time; the encoder
            // is many times slower fed that way, so buffer in front of it.
            let encoder = GzEncoder::new(buffered, Compression::default());
            let mut input = BufWriter::with_capacity(1 << 16, encoder);
            body(&mut input)?;
            let encoder = input.into_inner().map_err(|e| e.into_error())?;
            encoder.finish()?.flush()
        } else {
            body(&mut buffered)?;
            buffered.flush()
        }
    })
}

/// Create `out` as a new file with the contents `fill` writes. Returns its
/// size in bytes.
///
/// The contents go to a partial file beside `out`, which is given the final
/// name only once complete. So a run that fails or is killed part-way never
/// leaves a short file that looks finished, and a file that already has the
/// name, or takes it meanwhile, is never replaced.
pub(crate) fn create_then(out: &Path, fill: impl FnOnce(File) -> io::Result<()>) -> Result<u64> {
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
