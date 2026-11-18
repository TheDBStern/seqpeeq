//! `bam export-region` and `bam export-ref`: planned (every check, nothing
//! written) when the command starts, written by a job.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use noodles::bam;

use crate::bam::export_reads::{ReadsArgs, ReadsExport};
use crate::bam::index::{build_index, file_name, write_index};
use crate::bam::{BamDoc, BamSource, Reference, Region, grouped, parse_region};
use crate::command::bam::BamCmd;
use crate::msa::{Applied, count};
use crate::progress::Progress;
use crate::tree::full_path;
use crate::write::{create_text, create_then};
use crate::{Error, Result};

/// Bases per line in a reference stretch.
const WRAP: usize = 60;

/// An export, checked and ready to write.
pub(crate) enum BamExport {
    Region {
        source: BamSource,
        region: Region,
        cut: Option<String>,
        out: PathBuf,
    },
    Reads(Box<ReadsExport>),
    Ref {
        reference: Arc<Reference>,
        name: String,
        region: Region,
        cut: Option<String>,
        out: PathBuf,
    },
}

pub(crate) fn refuse_taken(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        return Err(Error::Exists(path.to_path_buf()));
    }
    Ok(())
}

pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

impl BamExport {
    /// Check `cmd` against `doc`; nothing is written.
    pub(crate) fn plan(doc: &BamDoc, cmd: &BamCmd) -> Result<BamExport> {
        let source = doc.source();
        let file = source.file_name();
        match cmd {
            BamCmd::ExportRegion { region, out } => {
                let (region, cut) = parse_region(region, source.contigs(), &file)?;
                refuse_taken(out)?;
                // The index is .bai or .csi, as the contigs' lengths decide.
                refuse_taken(&with_suffix(out, ".bai"))?;
                refuse_taken(&with_suffix(out, ".csi"))?;
                Ok(BamExport::Region {
                    source: source.clone(),
                    region,
                    cut,
                    out: out.clone(),
                })
            }
            BamCmd::ExportRef { region, out } => {
                let Some(reference) = source.reference.clone() else {
                    return Err(Error::Invalid(
                        "export-ref needs the reference: bam ref FILE".to_string(),
                    ));
                };
                let (region, cut) = parse_region(region, source.contigs(), &file)?;
                let name = source.contigs()[region.contig].name.clone();
                if !reference.has(&name) {
                    return Err(Error::Invalid(format!(
                        "{name} is not in {}",
                        file_name(reference.path())
                    )));
                }
                refuse_taken(out)?;
                Ok(BamExport::Ref {
                    reference,
                    name,
                    region,
                    cut,
                    out: out.clone(),
                })
            }
            BamCmd::ExportReads {
                region,
                names,
                names_file,
                out,
                with_mates,
                clip,
            } => Ok(BamExport::Reads(Box::new(ReadsExport::plan(
                source,
                ReadsArgs {
                    region,
                    names,
                    names_file: names_file.as_deref(),
                    out,
                    with_mates: *with_mates,
                    clip: *clip,
                },
            )?))),
            BamCmd::Ref { .. } => unreachable!("bam ref is not an export"),
        }
    }

    /// The file being written.
    pub(crate) fn out(&self) -> &Path {
        match self {
            BamExport::Region { out, .. } | BamExport::Ref { out, .. } => out,
            BamExport::Reads(reads) => reads.out(),
        }
    }

    pub(crate) fn run(&self, progress: &Progress) -> Result<Applied> {
        if progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let applied = match self {
            BamExport::Reads(reads) => reads.run(progress)?,
            BamExport::Region {
                source,
                region,
                cut,
                out,
            } => write_region(source, region, out, progress).map(|mut applied| {
                if let Some(cut) = cut {
                    applied.message.push_str(&format!("; {cut}"));
                }
                applied
            })?,
            BamExport::Ref {
                reference,
                name,
                region,
                cut,
                out,
            } => {
                let bases = reference.fetch(name, region.start, region.end)?;
                let label = region_label(name, region);
                let size = create_text(out, |w| {
                    writeln!(w, ">{label}")?;
                    for line in bases.chunks(WRAP) {
                        w.write_all(line)?;
                        w.write_all(b"\n")?;
                    }
                    Ok(())
                })?;
                let mut message = format!(
                    "wrote {label} ({} bp) to {}",
                    grouped(bases.len() as u64),
                    file_name(out)
                );
                if let Some(cut) = cut {
                    message.push_str(&format!("; {cut}"));
                }
                Applied {
                    message,
                    note: Some(format!("output {} size={size}", full_path(out).display())),
                    created: vec![out.clone()],
                }
            }
        };
        if progress.is_cancelled() {
            for path in &applied.created {
                let _ = std::fs::remove_file(path);
            }
            return Err(Error::Cancelled);
        }
        Ok(applied)
    }
}

/// `c1:101-110`.
fn region_label(name: &str, region: &Region) -> String {
    format!("{name}:{}-{}", region.start, region.end)
}

/// Write the records overlapping `region`, then the new file's index.
fn write_region(
    source: &BamSource,
    region: &Region,
    out: &Path,
    progress: &Progress,
) -> Result<Applied> {
    write_bam(source, out, progress, |put| {
        source.scan(region, progress, |record| {
            put(record)?;
            Ok(true)
        })
    })
}

/// Write a new BAM with `source`'s header and the records `fill` hands to
/// `put`, then its index beside it.
pub(crate) fn write_bam(
    source: &BamSource,
    out: &Path,
    progress: &Progress,
    fill: impl FnOnce(&mut dyn FnMut(&bam::Record) -> io::Result<()>) -> Result<()>,
) -> Result<Applied> {
    let mut records = 0usize;
    // An error from reading the source, kept whole (a cancel stays a cancel)
    // while the writer sees only that it failed.
    let mut reading: Option<Error> = None;
    let size = create_then(out, |file| {
        let mut writer = bam::io::Writer::new(file);
        writer.write_header(&source.header)?;
        let filled = fill(&mut |record| {
            records += 1;
            writer.write_record(&source.header, record)
        });
        if let Err(error) = filled {
            reading = Some(error);
            return Err(io::Error::other("reading the BAM failed"));
        }
        writer.try_finish()
    });
    let size = match (size, reading) {
        (_, Some(error)) => return Err(error),
        (result, None) => result?,
    };
    let index = match build_index(out, progress) {
        Ok(index) => index,
        Err(error) => {
            let _ = std::fs::remove_file(out);
            return Err(error);
        }
    };
    let index_path = with_suffix(
        out,
        match index {
            bam::Index::Bai(_) => ".bai",
            bam::Index::Csi(_) => ".csi",
        },
    );
    let index_size = match write_index(&index_path, &index) {
        Ok(size) => size,
        Err(error) => {
            let _ = std::fs::remove_file(out);
            return Err(error);
        }
    };
    Ok(Applied {
        message: format!(
            "wrote {} to {} and its index {}",
            count(records, "record"),
            file_name(out),
            file_name(&index_path)
        ),
        note: Some(format!(
            "output {} size={size}; output {} size={index_size}",
            full_path(out).display(),
            full_path(&index_path).display()
        )),
        created: vec![out.to_path_buf(), index_path],
    })
}
