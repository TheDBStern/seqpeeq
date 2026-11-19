//! BAM commands.

use std::path::PathBuf;

use clap::Subcommand;

use super::{Words, path_word};
use crate::Result;

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum BamCmd {
    /// Attach a reference FASTA to the current BAM, for reference bases,
    /// mismatches and export-ref.
    Ref { file: PathBuf },
    /// Write every record overlapping a region to a new BAM, with its index.
    ExportRegion {
        /// contig:start-end, or a contig's name for all of it
        region: String,
        #[arg(long, short)]
        out: PathBuf,
    },
    /// Write reads to a new BAM, FASTA or FASTQ (by --out's extension).
    ExportReads {
        /// contig:start-end, or a contig's name for all of it
        #[arg(
            long,
            required_unless_present = "everywhere",
            conflicts_with = "everywhere"
        )]
        region: Option<String>,
        /// Only reads with this name (repeat for more)
        #[arg(long = "name", allow_hyphen_values = true)]
        names: Vec<String>,
        /// Only reads named in this file, one per line
        #[arg(long, conflicts_with = "names")]
        names_file: Option<PathBuf>,
        #[arg(long, short)]
        out: PathBuf,
        /// Add each read's mate and the other parts of split reads
        #[arg(long)]
        with_mates: bool,
        /// Keep only the bases inside the region (FASTA or FASTQ)
        #[arg(long)]
        clip: bool,
        /// Search the whole BAM for the names instead of reading a region,
        /// writing every record found
        #[arg(long)]
        everywhere: bool,
    },
    /// Stack the current BAM with an open BAM that has the same contigs.
    Stack {
        /// The open BAM: its path, or just its file name
        file: PathBuf,
    },
    /// Split the current BAM's stack back into separate tabs.
    Unstack,
    /// Write a stretch of the reference to a new FASTA.
    ExportRef {
        /// contig:start-end, or a contig's name for all of it
        region: String,
        #[arg(long, short)]
        out: PathBuf,
    },
}

impl BamCmd {
    /// The command's short name, as typed after `bam`.
    pub fn name(&self) -> &'static str {
        match self {
            BamCmd::Ref { .. } => "ref",
            BamCmd::ExportRegion { .. } => "export-region",
            BamCmd::ExportRef { .. } => "export-ref",
            BamCmd::ExportReads { .. } => "export-reads",
            BamCmd::Stack { .. } => "stack",
            BamCmd::Unstack => "unstack",
        }
    }

    pub(super) fn write(&self, words: &mut Words) -> Result<()> {
        words.word(self.name());
        match self {
            BamCmd::Ref { file } | BamCmd::Stack { file } => words.positional(path_word(file)?),
            BamCmd::Unstack => {}
            BamCmd::ExportRegion { region, out } | BamCmd::ExportRef { region, out } => {
                words.flag("--out", path_word(out)?);
                words.positional(region);
            }
            BamCmd::ExportReads {
                region,
                names,
                names_file,
                out,
                with_mates,
                clip,
                everywhere,
            } => {
                if let Some(region) = region {
                    words.flag("--region", region);
                }
                for name in names {
                    words.flag("--name", name);
                }
                if let Some(file) = names_file {
                    words.flag("--names-file", path_word(file)?);
                }
                if *everywhere {
                    words.switch("--everywhere");
                }
                words.flag("--out", path_word(out)?);
                if *with_mates {
                    words.switch("--with-mates");
                }
                if *clip {
                    words.switch("--clip");
                }
            }
        }
        Ok(())
    }
}
