//! Tree commands.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::{Words, path_word};
use crate::Result;

/// One node of the current tree: the root of the clade a command acts on.
#[derive(Debug, Clone, PartialEq, Default, Args)]
#[group(multiple = false)]
pub struct NodeSel {
    /// A tip, by its exact name
    #[arg(long, allow_hyphen_values = true)]
    pub tip: Option<String>,
    /// Two tips, each given with --mrca; the clade is their most recent
    /// common ancestor
    #[arg(long, allow_hyphen_values = true)]
    pub mrca: Vec<String>,
    /// The Nth node in pre-order, counting from 1
    #[arg(long)]
    pub node: Option<usize>,
}

impl NodeSel {
    /// True when no node was named.
    pub fn is_empty(&self) -> bool {
        self.tip.is_none() && self.mrca.is_empty() && self.node.is_none()
    }

    fn write(&self, words: &mut Words) {
        if let Some(tip) = &self.tip {
            words.flag("--tip", tip);
        }
        for tip in &self.mrca {
            words.flag("--mrca", tip);
        }
        if let Some(node) = self.node {
            words.flag("--node", node);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum TreeCmd {
    /// Write a clade, or the whole tree, to a new file; with --seqs, also
    /// its tips' sequences from the linked alignment.
    #[command(alias = "save")]
    Extract {
        #[command(flatten)]
        at: NodeSel,
        #[arg(long, short)]
        out: PathBuf,
        /// A new FASTA file for the clade's sequences
        #[arg(long)]
        seqs: Option<PathBuf>,
        /// Residues per line in the sequence file
        #[arg(long)]
        wrap: Option<usize>,
    },
    /// Remove a clade.
    Prune {
        #[command(flatten)]
        at: NodeSel,
    },
    /// Root on the branch above a node, between an outgroup and the rest,
    /// or at the midpoint of the longest path between tips.
    Reroot {
        #[command(flatten)]
        at: NodeSel,
        /// A tip of the outgroup; give each with --outgroup
        #[arg(long, allow_hyphen_values = true)]
        outgroup: Vec<String>,
        #[arg(long)]
        midpoint: bool,
    },
    /// Order every node's children by tip count, fewest first.
    Ladderize {
        /// Most tips first
        #[arg(long)]
        reverse: bool,
    },
    /// Rename one tip with `--to`, or many with `--regex` and `--replace`.
    Rename {
        #[command(flatten)]
        at: NodeSel,
        #[arg(long, allow_hyphen_values = true, conflicts_with_all = ["regex", "replace"])]
        to: Option<String>,
        #[arg(long, allow_hyphen_values = true, requires = "replace")]
        regex: Option<String>,
        #[arg(long, allow_hyphen_values = true, requires = "regex")]
        replace: Option<String>,
    },
    /// Pair the tree with an open alignment, matching tip and sequence names.
    Link { file: PathBuf },
    /// Drop the tree's link to an alignment.
    Unlink,
}

impl TreeCmd {
    /// The command's short name, as typed after `tree`.
    pub fn name(&self) -> &'static str {
        match self {
            TreeCmd::Extract { .. } => "extract",
            TreeCmd::Prune { .. } => "prune",
            TreeCmd::Reroot { .. } => "reroot",
            TreeCmd::Ladderize { .. } => "ladderize",
            TreeCmd::Rename { .. } => "rename",
            TreeCmd::Link { .. } => "link",
            TreeCmd::Unlink => "unlink",
        }
    }

    pub(super) fn write(&self, words: &mut Words) -> Result<()> {
        words.word(self.name());
        match self {
            TreeCmd::Extract {
                at,
                out,
                seqs,
                wrap,
            } => {
                at.write(words);
                words.flag("--out", path_word(out)?);
                if let Some(seqs) = seqs {
                    words.flag("--seqs", path_word(seqs)?);
                }
                if let Some(wrap) = wrap {
                    words.flag("--wrap", wrap);
                }
            }
            TreeCmd::Prune { at } => at.write(words),
            TreeCmd::Reroot {
                at,
                outgroup,
                midpoint,
            } => {
                at.write(words);
                for tip in outgroup {
                    words.flag("--outgroup", tip);
                }
                if *midpoint {
                    words.switch("--midpoint");
                }
            }
            TreeCmd::Ladderize { reverse } => {
                if *reverse {
                    words.switch("--reverse");
                }
            }
            TreeCmd::Rename {
                at,
                to,
                regex,
                replace,
            } => {
                at.write(words);
                if let Some(to) = to {
                    words.flag("--to", to);
                }
                if let Some(regex) = regex {
                    words.flag("--regex", regex);
                }
                if let Some(replace) = replace {
                    words.flag("--replace", replace);
                }
            }
            TreeCmd::Link { file } => words.positional(path_word(file)?),
            TreeCmd::Unlink => {}
        }
        Ok(())
    }
}
