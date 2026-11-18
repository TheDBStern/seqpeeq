//! Finding a BAM's index, or building one: beside the BAM when its folder
//! can be written, otherwise in the cache folder.

use std::fs::File;
use std::io::{self, BufWriter, Write as _};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use noodles::bam;
use noodles::core::Position;
use noodles::csi;
use noodles::csi::binning_index::index::reference_sequence::bin::Chunk;
use noodles::sam;
use noodles::sam::alignment::Record as _;

use crate::progress::Progress;
use crate::write::create_then;
use crate::{Error, Result};

/// Records read between checks for cancelling.
const BATCH: u64 = 4096;

/// The refusal for a BAM not sorted by position.
pub(crate) fn unsorted(bam: &Path) -> Error {
    Error::Invalid(format!(
        "{} is not sorted by position (samtools sort makes a sorted copy)",
        file_name(bam)
    ))
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> Error + '_ {
    move |cause| Error::Io {
        path: path.to_path_buf(),
        cause,
    }
}

/// The index files a BAM may already have, in the order looked for.
fn beside(bam: &Path) -> [PathBuf; 3] {
    let with = |ext: &str| {
        let mut name = bam.as_os_str().to_owned();
        name.push(ext);
        PathBuf::from(name)
    };
    [with(".bai"), bam.with_extension("bai"), with(".csi")]
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// FNV-1a, 64-bit: a hash that stays the same across Rust versions, so a
/// cached index keeps its name.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Where the cache keeps this BAM's index, without its extension: named
/// after the BAM's full path, size and modification time, so a changed BAM
/// gets a new one.
fn cache_stem(bam: &Path, cache: &Path) -> Result<PathBuf> {
    let full = std::fs::canonicalize(bam).map_err(io_error(bam))?;
    let meta = std::fs::metadata(bam).map_err(io_error(bam))?;
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut key = full.as_os_str().as_encoded_bytes().to_vec();
    key.extend_from_slice(&meta.len().to_le_bytes());
    key.extend_from_slice(&nanos.to_le_bytes());
    Ok(cache.join(format!("{}-{:016x}", file_name(bam), fnv1a(&key))))
}

fn with_ext(stem: &Path, index: &bam::Index) -> PathBuf {
    let mut name = stem.as_os_str().to_owned();
    name.push(match index {
        bam::Index::Bai(_) => ".bai",
        bam::Index::Csi(_) => ".csi",
    });
    PathBuf::from(name)
}

fn read_index(path: &Path) -> Result<bam::Index> {
    let csi = path.extension().is_some_and(|e| e == "csi");
    let read = if csi {
        csi::fs::read(path).map(bam::Index::Csi)
    } else {
        bam::bai::fs::read(path).map(bam::Index::Bai)
    };
    read.map_err(|e| Error::Format {
        path: path.to_path_buf(),
        message: format!("not a readable index: {e}"),
    })
}

/// Write `index` as the new file `path`; returns its size.
pub(crate) fn write_index(path: &Path, index: &bam::Index) -> Result<u64> {
    create_then(path, |file| match index {
        bam::Index::Bai(bai) => {
            let mut writer = bam::bai::io::Writer::new(BufWriter::new(file));
            writer.write_index(bai)?;
            writer.get_mut().flush()
        }
        bam::Index::Csi(index) => {
            let mut writer = csi::io::Writer::new(file);
            writer.write_index(index)?;
            writer.into_inner().finish().map(drop)
        }
    })
}

/// The BAM's index, read from beside it or from the cache, or built; with
/// a note saying where a built one went.
pub(crate) fn load_or_build(
    bam: &Path,
    cache: &Path,
    progress: &Progress,
) -> Result<(bam::Index, Option<String>)> {
    let bam_time = modified(bam);
    let mut stale = None;
    for path in beside(bam) {
        if path.exists() {
            if modified(&path) >= bam_time {
                return Ok((read_index(&path)?, None));
            }
            stale.get_or_insert(path);
        }
    }
    let stem = cache_stem(bam, cache)?;
    for ext in [".bai", ".csi"] {
        let mut name = stem.as_os_str().to_owned();
        name.push(ext);
        let cached = PathBuf::from(name);
        if cached.exists() {
            return Ok((read_index(&cached)?, None));
        }
    }
    let index = build_index(bam, progress)?;
    let to_cache = |index: &bam::Index| -> Result<PathBuf> {
        std::fs::create_dir_all(cache).map_err(io_error(cache))?;
        let path = with_ext(&stem, index);
        write_index(&path, index)?;
        Ok(path)
    };
    let note = match stale {
        Some(old) => {
            to_cache(&index)?;
            format!(
                "{} is older than {}; built a fresh index in the cache",
                file_name(&old),
                file_name(bam)
            )
        }
        None => {
            let mut name = bam.as_os_str().to_owned();
            name.push(match index {
                bam::Index::Bai(_) => ".bai",
                bam::Index::Csi(_) => ".csi",
            });
            let next_to = PathBuf::from(name);
            match write_index(&next_to, &index) {
                Ok(_) => format!("built the index {}", file_name(&next_to)),
                Err(_) => {
                    let cached = to_cache(&index)?;
                    format!(
                        "{}'s folder is read-only, so the index went to {}",
                        file_name(bam),
                        cached.display()
                    )
                }
            }
        }
    };
    Ok((index, Some(note)))
}

/// Build the index of a BAM sorted by position, as noodles' own indexer
/// does, but without demanding `SO:coordinate` in the header: the records'
/// order is checked instead.
pub(crate) fn build_index(path: &Path, progress: &Progress) -> Result<bam::Index> {
    let failed = io_error(path);
    let file = File::open(path).map_err(&failed)?;
    let total = file.metadata().map_err(&failed)?.len();
    let mut reader = bam::io::Reader::new(file);
    let header = reader.read_header().map_err(&failed)?;
    progress.set_total(total);

    let longest = header
        .reference_sequences()
        .values()
        .map(|rs| rs.length())
        .max()
        .unwrap_or(NonZero::<usize>::MIN);
    let max_position = Position::new(longest.get()).expect("a nonzero length");
    let mut indexer = bam::index::Indexer::builder()
        .set_max_position_hint(max_position)
        .build()
        .map_err(|e| Error::Format {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    let mut record = bam::Record::default();
    let mut start = reader.get_ref().virtual_position();
    let mut last: Option<(usize, Option<Position>)> = None;
    let mut unplaced = false;
    let mut done: u64 = 0;
    let mut counted = 0;
    while reader.read_record(&mut record).map_err(&failed)? != 0 {
        let end = reader.get_ref().virtual_position();
        let id = record
            .reference_sequence_id()
            .transpose()
            .map_err(&failed)?;
        let pos = record.alignment_start().transpose().map_err(&failed)?;
        match id {
            None => unplaced = true,
            Some(id) => {
                let out_of_order = unplaced
                    || last.is_some_and(|(last_id, last_pos)| {
                        id < last_id || (id == last_id && pos < last_pos)
                    });
                if out_of_order {
                    return Err(unsorted(path));
                }
                last = Some((id, pos));
            }
        }
        let context = context(&record).map_err(&failed)?;
        indexer
            .add_record(context, Chunk::new(start, end))
            .map_err(|e| Error::Format {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        start = end;
        counted += 1;
        if counted % BATCH == 0 {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let at = end.compressed();
            progress.add(at - done);
            done = at;
        }
    }
    if progress.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(indexer.build(header.reference_sequences().len()))
}

type Context = Option<(usize, Position, Position, bool)>;

fn context(record: &bam::Record) -> io::Result<Context> {
    match (
        record.reference_sequence_id().transpose()?,
        record.alignment_start().transpose()?,
        record.alignment_end().transpose()?,
    ) {
        (Some(id), Some(start), Some(end)) => {
            Ok(Some((id, start, end, !record.flags().is_unmapped())))
        }
        _ => Ok(None),
    }
}

/// Whether the header says the records are in some order other than by
/// position.
pub(crate) fn says_unsorted(header: &sam::Header) -> bool {
    use sam::header::record::value::map::header::tag::SORT_ORDER;
    header
        .header()
        .and_then(|h| h.other_fields().get(&SORT_ORDER))
        .is_some_and(|order| order == "unsorted" || order == "queryname")
}
