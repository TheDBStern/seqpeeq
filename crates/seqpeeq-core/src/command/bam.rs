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
}

impl BamCmd {
    /// The command's short name, as typed after `bam`.
    pub fn name(&self) -> &'static str {
        match self {
            BamCmd::Ref { .. } => "ref",
        }
    }

    pub(super) fn write(&self, words: &mut Words) -> Result<()> {
        words.word(self.name());
        match self {
            BamCmd::Ref { file } => words.positional(path_word(file)?),
        }
        Ok(())
    }
}
