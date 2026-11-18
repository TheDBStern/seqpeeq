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
        }
    }

    pub(super) fn write(&self, words: &mut Words) -> Result<()> {
        words.word(self.name());
        match self {
            BamCmd::Ref { file } => words.positional(path_word(file)?),
            BamCmd::ExportRegion { region, out } | BamCmd::ExportRef { region, out } => {
                words.flag("--out", path_word(out)?);
                words.positional(region);
            }
        }
        Ok(())
    }
}
