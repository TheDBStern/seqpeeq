//! `bam export-reads`: a region's reads (or some of them, by name) to a new
//! BAM, FASTA or FASTQ, with their mates and split parts if asked.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use noodles::bam;

use crate::bam::export::{refuse_taken, with_suffix, write_bam};
use crate::bam::index::file_name;
use crate::bam::read::cigar_of;
use crate::bam::{BamSource, Region, parse_region};
use crate::msa::{Applied, count};
use crate::progress::Progress;
use crate::tree::full_path;
use crate::write::create_text;
use crate::{Error, Result};

const PAIRED: u16 = 0x1;
const UNMAPPED: u16 = 0x4;
const REVERSE: u16 = 0x10;
const FIRST: u16 = 0x40;
const LAST: u16 = 0x80;
const SECONDARY: u16 = 0x100;
const SUPPLEMENTARY: u16 = 0x800;

/// Records written between checks for cancelling.
const BATCH: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Bam,
    Fasta,
    Fastq,
}

/// The output format, from `out`'s name.
fn format_of(out: &Path) -> Result<Format> {
    let name = file_name(out).to_ascii_lowercase();
    let name = name.strip_suffix(".gz").unwrap_or(&name);
    let ext = name.rsplit_once('.').map(|(_, ext)| ext);
    match ext {
        Some("bam") => Ok(Format::Bam),
        Some("fa" | "fasta" | "fna") => Ok(Format::Fasta),
        Some("fq" | "fastq") => Ok(Format::Fastq),
        _ => Err(Error::Invalid(
            "--out must end in .bam, .fa, .fasta, .fna, .fq or .fastq (or those with .gz)"
                .to_string(),
        )),
    }
}

/// The names in a names file, one per line; blank lines are skipped.
fn read_names(file: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(file).map_err(|cause| Error::Io {
        path: file.to_path_buf(),
        cause,
    })?;
    let names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Err(Error::Invalid(format!(
            "{} names no reads",
            file_name(file)
        )));
    }
    Ok(names)
}

/// The arguments of `bam export-reads`, as the command gives them.
pub(crate) struct ReadsArgs<'a> {
    pub region: &'a str,
    pub names: &'a [String],
    pub names_file: Option<&'a Path>,
    pub out: &'a Path,
    pub with_mates: bool,
    pub clip: bool,
}

/// `bam export-reads`, checked and ready to write.
pub(crate) struct ReadsExport {
    source: BamSource,
    region: Region,
    cut: Option<String>,
    /// Only these names; `None` for every read.
    names: Option<Vec<String>>,
    out: PathBuf,
    format: Format,
    with_mates: bool,
    clip: bool,
}

/// What is said after the count of reads written.
#[derive(Default)]
struct Tally {
    written: usize,
    secondary: usize,
    from_part: usize,
    not_found: usize,
    no_base: usize,
}

/// A read's identity: its name and which segment of a pair it is (0 when
/// it is not paired).
type Key = (Vec<u8>, u8);

fn name_of(record: &bam::Record) -> Vec<u8> {
    record.name().map(|n| n.to_vec()).unwrap_or_default()
}

fn segment(flags: u16) -> u8 {
    if flags & PAIRED == 0 {
        0
    } else if flags & FIRST != 0 {
        1
    } else if flags & LAST != 0 {
        2
    } else {
        0
    }
}

fn key_of(record: &bam::Record) -> Key {
    (name_of(record), segment(record.flags().bits()))
}

fn start_of(record: &bam::Record) -> io::Result<Option<u64>> {
    Ok(record
        .alignment_start()
        .transpose()?
        .map(|p| p.get() as u64))
}

fn complement(base: u8) -> u8 {
    match base {
        b'A' => b'T',
        b'C' => b'G',
        b'G' => b'C',
        b'T' => b'A',
        b'a' => b't',
        b'c' => b'g',
        b'g' => b'c',
        b't' => b'a',
        other => other,
    }
}

/// A record's bases and FASTQ qualities as stored (on the reference's
/// strand); missing qualities are `!`.
fn bases_and_qualities(record: &bam::Record) -> (Vec<u8>, Vec<u8>) {
    let bases: Vec<u8> = record.sequence().iter().collect();
    let scores = record.quality_scores();
    let qualities = if scores.len() == bases.len() {
        scores.iter().map(|q| q.min(93) + 33).collect()
    } else {
        vec![b'!'; bases.len()]
    };
    (bases, qualities)
}

/// The read indices of the bases aligned inside `start..=end` (M, = and X,
/// plus insertions between two such bases), and the reference span they
/// cover there; `None` when no base is inside.
fn clipped(
    record: &bam::Record,
    start: u64,
    end: u64,
) -> io::Result<Option<(Vec<usize>, u64, u64)>> {
    let Some(mut pos) = start_of(record)? else {
        return Ok(None);
    };
    let mut read = 0usize;
    let mut kept = Vec::new();
    let mut span: Option<(u64, u64)> = None;
    let mut last_inside = false;
    let mut pending: Option<std::ops::Range<usize>> = None;
    for (op, len) in cigar_of(record)? {
        let len = len as usize;
        match op {
            'M' | '=' | 'X' => {
                for _ in 0..len {
                    let inside = (start..=end).contains(&pos);
                    if let Some(insertion) = pending.take()
                        && inside
                        && last_inside
                    {
                        kept.extend(insertion);
                    }
                    if inside {
                        kept.push(read);
                        span = Some(span.map_or((pos, pos), |(a, _)| (a, pos)));
                    }
                    last_inside = inside;
                    pos += 1;
                    read += 1;
                }
            }
            'I' => {
                pending = last_inside.then_some(read..read + len);
                read += len;
            }
            'S' => {
                pending = None;
                read += len;
            }
            'D' | 'N' => pos += len as u64,
            _ => {}
        }
    }
    Ok(span.map(|(a, b)| (kept, a, b)))
}

impl ReadsExport {
    pub(crate) fn plan(source: &BamSource, args: ReadsArgs) -> Result<ReadsExport> {
        let format = format_of(args.out)?;
        if args.clip && format == Format::Bam {
            return Err(Error::Invalid(
                "--clip is for FASTA or FASTQ output".to_string(),
            ));
        }
        if args.clip && args.with_mates {
            return Err(Error::Invalid(
                "--clip keeps only the bases inside the region, so it cannot take --with-mates"
                    .to_string(),
            ));
        }
        let names = match args.names_file {
            Some(file) => Some(read_names(file)?),
            None if args.names.is_empty() => None,
            None => Some(args.names.to_vec()),
        };
        let (region, cut) = parse_region(args.region, source.contigs(), &source.file_name())?;
        refuse_taken(args.out)?;
        if format == Format::Bam {
            refuse_taken(&with_suffix(args.out, ".bai"))?;
            refuse_taken(&with_suffix(args.out, ".csi"))?;
        }
        Ok(ReadsExport {
            source: source.clone(),
            region,
            cut,
            names,
            out: args.out.to_path_buf(),
            format,
            with_mates: args.with_mates,
            clip: args.clip,
        })
    }

    pub(crate) fn out(&self) -> &Path {
        &self.out
    }

    pub(crate) fn run(&self, progress: &Progress) -> Result<Applied> {
        let wanted: Option<HashSet<&[u8]>> = self
            .names
            .as_ref()
            .map(|names| names.iter().map(|n| n.as_bytes()).collect());
        let mut matched: HashSet<Vec<u8>> = HashSet::new();
        let mut tally = Tally::default();
        // Is this record one of the selection? Mapped, and named if names
        // were given.
        let mut select = |record: &bam::Record| -> bool {
            if record.flags().bits() & UNMAPPED != 0 {
                return false;
            }
            match &wanted {
                None => true,
                Some(wanted) => {
                    let name = name_of(record);
                    let hit = wanted.contains(name.as_slice());
                    if hit {
                        matched.insert(name);
                    }
                    hit
                }
            }
        };

        let mut applied = if self.with_mates {
            let mut selected = Vec::new();
            self.source.scan(&self.region, progress, |record| {
                if select(record) {
                    selected.push(record.clone());
                }
                Ok(true)
            })?;
            let added = self.mates_and_parts(&selected, &mut tally, progress)?;
            match self.format {
                Format::Bam => {
                    let mut all: Vec<&bam::Record> = selected.iter().chain(&added).collect();
                    let mut seen = HashSet::new();
                    let mut unique = Vec::with_capacity(all.len());
                    for record in all.drain(..) {
                        let id = (
                            name_of(record),
                            record.flags().bits(),
                            record.reference_sequence_id().transpose().ok().flatten(),
                            start_of(record).ok().flatten(),
                        );
                        if seen.insert(id) {
                            unique.push(record);
                        }
                    }
                    // Placed records by contig and position; any others last.
                    unique.sort_by_key(|record| {
                        let contig = record.reference_sequence_id().transpose().ok().flatten();
                        (contig.is_none(), contig, start_of(record).ok().flatten())
                    });
                    write_bam(&self.source, &self.out, progress, |put| {
                        for (i, record) in unique.iter().enumerate() {
                            if i % BATCH == BATCH - 1 && progress.is_cancelled() {
                                return Err(Error::Cancelled);
                            }
                            put(record).map_err(|cause| self.write_error(cause))?;
                        }
                        Ok(())
                    })?
                }
                Format::Fasta | Format::Fastq => self.write_text(&mut tally, progress, |emit| {
                    for (i, record) in selected.iter().chain(&added).enumerate() {
                        if i % BATCH == BATCH - 1 && progress.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        emit(record)?;
                    }
                    Ok(())
                })?,
            }
        } else {
            match self.format {
                Format::Bam => write_bam(&self.source, &self.out, progress, |put| {
                    self.source.scan(&self.region, progress, |record| {
                        if select(record) {
                            put(record)?;
                        }
                        Ok(true)
                    })
                })?,
                Format::Fasta | Format::Fastq => self.write_text(&mut tally, progress, |emit| {
                    // Reading stops at the first failure to write, which is
                    // kept whole.
                    let mut failed = None;
                    self.source.scan(&self.region, progress, |record| {
                        if !select(record) {
                            return Ok(true);
                        }
                        match emit(record) {
                            Ok(()) => Ok(true),
                            Err(error) => {
                                failed = Some(error);
                                Ok(false)
                            }
                        }
                    })?;
                    failed.map_or(Ok(()), Err)
                })?,
            }
        };
        if progress.is_cancelled() {
            for path in &applied.created {
                let _ = std::fs::remove_file(path);
            }
            return Err(Error::Cancelled);
        }
        let mut extra = String::new();
        if let Some(names) = &self.names {
            let distinct: HashSet<&str> = names.iter().map(String::as_str).collect();
            let missing = distinct
                .iter()
                .filter(|n| !matched.contains(n.as_bytes()))
                .count();
            if missing > 0 {
                let had = if missing == 1 {
                    "name had"
                } else {
                    "names had"
                };
                extra.push_str(&format!("; {missing} {had} no read in the region"));
            }
        }
        if tally.secondary > 0 {
            extra.push_str(&format!(
                "; left out {}",
                count(tally.secondary, "secondary alignment")
            ));
        }
        if tally.from_part > 0 {
            extra.push_str(&format!(
                "; {} written from a split part (no primary found)",
                count(tally.from_part, "read")
            ));
        }
        if tally.not_found > 0 {
            let what = if tally.not_found == 1 {
                "mate or part"
            } else {
                "mates or parts"
            };
            extra.push_str(&format!("; {} {what} not found", tally.not_found));
        }
        if tally.no_base > 0 {
            let had = if tally.no_base == 1 {
                "read had"
            } else {
                "reads had"
            };
            extra.push_str(&format!("; {} {had} no base in the region", tally.no_base));
        }
        if let Some(cut) = &self.cut {
            extra.push_str(&format!("; {cut}"));
        }
        applied.message.push_str(&extra);
        Ok(applied)
    }

    fn write_error(&self, cause: io::Error) -> Error {
        Error::Io {
            path: self.out.clone(),
            cause,
        }
    }

    fn read_error(&self, cause: io::Error) -> Error {
        Error::Io {
            path: self.source.path().to_path_buf(),
            cause,
        }
    }

    /// The first record placed at `contig:pos` that `want` accepts.
    fn find_at(
        &self,
        contig: usize,
        pos: u64,
        progress: &Progress,
        want: impl Fn(&bam::Record) -> bool,
    ) -> Result<Option<bam::Record>> {
        let length = self.source.contigs()[contig].length;
        if pos == 0 || pos > length {
            return Ok(None);
        }
        let region = Region {
            contig,
            start: pos,
            end: pos,
        };
        let mut found = None;
        self.source.scan(&region, progress, |record| {
            if start_of(record)? == Some(pos) && want(record) {
                found = Some(record.clone());
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(found)
    }

    /// Each selected record's mate and the other parts of its read, in the
    /// order found, looked for among the selected records first.
    fn mates_and_parts(
        &self,
        selected: &[bam::Record],
        tally: &mut Tally,
        progress: &Progress,
    ) -> Result<Vec<bam::Record>> {
        let placed = |record: &bam::Record| -> Option<(usize, u64)> {
            let contig = record.reference_sequence_id().transpose().ok()??;
            Some((contig, start_of(record).ok()??))
        };
        let mut added: Vec<bam::Record> = Vec::new();
        let mut look = |contig: usize,
                        pos: u64,
                        tally: &mut Tally,
                        want: &dyn Fn(&bam::Record) -> bool|
         -> Result<()> {
            let here = |record: &bam::Record| placed(record) == Some((contig, pos)) && want(record);
            if selected.iter().chain(added.iter()).any(here) {
                return Ok(());
            }
            match self.find_at(contig, pos, progress, want)? {
                Some(record) => added.push(record),
                None => tally.not_found += 1,
            }
            Ok(())
        };
        for record in selected {
            let flags = record.flags().bits();
            if flags & SECONDARY != 0 {
                continue;
            }
            let name = name_of(record);
            let seg = segment(flags);
            let mate_contig = record
                .mate_reference_sequence_id()
                .transpose()
                .map_err(|e| self.read_error(e))?;
            let mate_pos = record
                .mate_alignment_start()
                .transpose()
                .map_err(|e| self.read_error(e))?;
            if flags & PAIRED != 0
                && let (Some(contig), Some(pos)) = (mate_contig, mate_pos)
            {
                // A mate flagged unmapped but placed beside its pair is
                // still found and written.
                let other = if seg == 1 { 2 } else { 1 };
                let want = |r: &bam::Record| {
                    let f = r.flags().bits();
                    name_of(r) == name
                        && segment(f) == other
                        && f & (SECONDARY | SUPPLEMENTARY) == 0
                };
                look(contig, pos.get() as u64, tally, &want)?;
            }
            for part in self
                .source
                .sa_parts(record)
                .map_err(|e| self.read_error(e))?
            {
                let want = |r: &bam::Record| name_of(r) == name && segment(r.flags().bits()) == seg;
                look(part.place.contig, part.place.pos, tally, &want)?;
            }
        }
        Ok(added)
    }

    /// Write a FASTA or FASTQ, `fill` handing each record to `emit`.
    fn write_text(
        &self,
        tally: &mut Tally,
        progress: &Progress,
        fill: impl FnOnce(&mut dyn FnMut(&bam::Record) -> Result<()>) -> Result<()>,
    ) -> Result<Applied> {
        let mut seen: HashSet<Key> = HashSet::new();
        let mut failure: Option<Error> = None;
        let size = create_text(&self.out, |w| {
            let filled = fill(&mut |record| self.emit(w, record, &mut seen, tally, progress));
            if let Err(error) = filled {
                failure = Some(error);
                return Err(io::Error::other("export-reads failed"));
            }
            Ok(())
        });
        let size = match (size, failure) {
            (_, Some(error)) => return Err(error),
            (result, None) => result?,
        };
        Ok(Applied {
            message: format!(
                "wrote {} to {}",
                count(tally.written, "read"),
                file_name(&self.out)
            ),
            note: Some(format!(
                "output {} size={size}",
                full_path(&self.out).display()
            )),
            created: vec![self.out.clone()],
        })
    }

    /// Write one read for `record`, unless it is secondary or its read was
    /// written already.
    fn emit(
        &self,
        w: &mut dyn Write,
        record: &bam::Record,
        seen: &mut HashSet<Key>,
        tally: &mut Tally,
        progress: &Progress,
    ) -> Result<()> {
        let flags = record.flags().bits();
        if flags & SECONDARY != 0 {
            tally.secondary += 1;
            return Ok(());
        }
        let key = key_of(record);
        if !seen.insert(key.clone()) {
            return Ok(());
        }
        let mut label = String::from_utf8_lossy(&key.0).into_owned();
        if key.1 > 0 {
            label.push_str(&format!("/{}", key.1));
        }
        let (bases, qualities) = if self.clip {
            let (bases, qualities) = bases_and_qualities(record);
            let inside = clipped(record, self.region.start, self.region.end)
                .map_err(|e| self.read_error(e))?;
            let Some((kept, a, b)) = inside else {
                tally.no_base += 1;
                return Ok(());
            };
            label.push_str(&format!(":{a}-{b}"));
            let pick = |from: &[u8]| kept.iter().filter_map(|&i| from.get(i).copied()).collect();
            (pick(&bases), pick(&qualities))
        } else {
            let primary = if flags & SUPPLEMENTARY != 0 {
                self.primary_of(record, &key, progress)?
            } else {
                None
            };
            if flags & SUPPLEMENTARY != 0 && primary.is_none() {
                tally.from_part += 1;
            }
            let from = primary.as_ref().unwrap_or(record);
            let (mut bases, mut qualities) = bases_and_qualities(from);
            if from.flags().bits() & REVERSE != 0 {
                bases.reverse();
                bases.iter_mut().for_each(|b| *b = complement(*b));
                qualities.reverse();
            }
            (bases, qualities)
        };
        let written = if self.format == Format::Fastq {
            w.write_all(b"@")
                .and_then(|()| w.write_all(label.as_bytes()))
                .and_then(|()| w.write_all(b"\n"))
                .and_then(|()| w.write_all(&bases))
                .and_then(|()| w.write_all(b"\n+\n"))
                .and_then(|()| w.write_all(&qualities))
                .and_then(|()| w.write_all(b"\n"))
        } else {
            w.write_all(b">")
                .and_then(|()| w.write_all(label.as_bytes()))
                .and_then(|()| w.write_all(b"\n"))
                .and_then(|()| w.write_all(&bases))
                .and_then(|()| w.write_all(b"\n"))
        };
        written.map_err(|cause| self.write_error(cause))?;
        tally.written += 1;
        Ok(())
    }

    /// The primary record of a supplementary one, at its SA tag's first
    /// entry.
    fn primary_of(
        &self,
        record: &bam::Record,
        key: &Key,
        progress: &Progress,
    ) -> Result<Option<bam::Record>> {
        let parts = self
            .source
            .sa_parts(record)
            .map_err(|e| self.read_error(e))?;
        let Some(first) = parts.first() else {
            return Ok(None);
        };
        self.find_at(first.place.contig, first.place.pos, progress, |r| {
            r.flags().bits() & (SECONDARY | SUPPLEMENTARY) == 0 && &key_of(r) == key
        })
    }
}
