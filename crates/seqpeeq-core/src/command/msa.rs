//! Commands that act on an alignment.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::{Words, path_word};
use crate::Result;
use crate::ranges::Ranges;

/// Which sequences a command applies to. At most one way of choosing may be
/// used; choosing nothing means every sequence where that makes sense.
#[derive(Debug, Clone, PartialEq, Default, Args)]
#[group(multiple = false)]
pub struct RowSel {
    /// Row numbers, such as 3-7,12
    #[arg(long)]
    pub rows: Option<Ranges>,
    /// A sequence name, matched exactly; may be given more than once
    #[arg(long, allow_hyphen_values = true)]
    pub name: Vec<String>,
    /// A regular expression searched for in sequence names
    #[arg(long = "match", allow_hyphen_values = true)]
    pub matching: Option<String>,
}

impl RowSel {
    /// True when nothing was chosen.
    pub fn is_empty(&self) -> bool {
        self.rows.is_none() && self.name.is_empty() && self.matching.is_none()
    }

    fn write(&self, words: &mut Words) {
        if let Some(rows) = &self.rows {
            words.flag("--rows", rows);
        }
        for name in &self.name {
            words.flag("--name", name);
        }
        if let Some(matching) = &self.matching {
            words.flag("--match", matching);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum MsaCmd {
    /// Remove sequences.
    DeleteRows {
        #[command(flatten)]
        rows: RowSel,
    },
    /// Remove columns.
    DeleteCols {
        #[arg(long)]
        cols: Ranges,
    },
    /// Overwrite a block with one character, such as `-` or `N`.
    Fill {
        #[command(flatten)]
        rows: RowSel,
        #[arg(long)]
        cols: Option<Ranges>,
        #[arg(long = "with", allow_hyphen_values = true)]
        with: char,
    },
    /// Replace residues in one row, starting at a column, with the
    /// characters given.
    Overwrite {
        #[arg(long)]
        row: usize,
        #[arg(long)]
        col: usize,
        #[arg(long, allow_hyphen_values = true)]
        seq: String,
    },
    /// Insert characters into one row at a column. The residues after it
    /// move right as far as the gaps it uses up; gap columns are added at
    /// the end when the row has too few.
    Insert {
        #[arg(long)]
        row: usize,
        #[arg(long)]
        col: usize,
        #[arg(long, allow_hyphen_values = true)]
        seq: String,
    },
    /// Remove characters from one row at a column. The residues after them
    /// move left as far as the next gap, leaving gaps behind.
    Remove {
        #[arg(long)]
        row: usize,
        #[arg(long)]
        col: usize,
        #[arg(long)]
        count: usize,
    },
    /// Write sequences, or parts of them, to a new FASTA file.
    #[command(alias = "save")]
    Extract {
        #[command(flatten)]
        rows: RowSel,
        #[arg(long)]
        cols: Option<Ranges>,
        /// Residues per line; one line per sequence when absent
        #[arg(long)]
        wrap: Option<usize>,
        #[arg(long, short)]
        out: PathBuf,
    },
    /// Rename one sequence with `--to`, or many with `--regex` and `--replace`.
    Rename {
        #[command(flatten)]
        rows: RowSel,
        #[arg(long, allow_hyphen_values = true, conflicts_with_all = ["regex", "replace"])]
        to: Option<String>,
        #[arg(long, allow_hyphen_values = true, requires = "replace")]
        regex: Option<String>,
        #[arg(long, allow_hyphen_values = true, requires = "regex")]
        replace: Option<String>,
        /// Also rename the matching tips in the linked tree
        #[arg(long)]
        tree: bool,
    },
}

impl MsaCmd {
    /// The command's short name, as typed after `msa`.
    pub fn name(&self) -> &'static str {
        match self {
            MsaCmd::DeleteRows { .. } => "delete-rows",
            MsaCmd::DeleteCols { .. } => "delete-cols",
            MsaCmd::Fill { .. } => "fill",
            MsaCmd::Overwrite { .. } => "overwrite",
            MsaCmd::Insert { .. } => "insert",
            MsaCmd::Remove { .. } => "remove",
            MsaCmd::Extract { .. } => "extract",
            MsaCmd::Rename { .. } => "rename",
        }
    }

    pub(super) fn write(&self, words: &mut Words) -> Result<()> {
        words.word(self.name());
        match self {
            MsaCmd::DeleteRows { rows } => rows.write(words),
            MsaCmd::DeleteCols { cols } => words.flag("--cols", cols),
            MsaCmd::Fill { rows, cols, with } => {
                rows.write(words);
                if let Some(cols) = cols {
                    words.flag("--cols", cols);
                }
                words.flag("--with", with);
            }
            MsaCmd::Overwrite { row, col, seq } | MsaCmd::Insert { row, col, seq } => {
                words.flag("--row", row);
                words.flag("--col", col);
                words.flag("--seq", seq);
            }
            MsaCmd::Remove { row, col, count } => {
                words.flag("--row", row);
                words.flag("--col", col);
                words.flag("--count", count);
            }
            MsaCmd::Extract {
                rows,
                cols,
                wrap,
                out,
            } => {
                rows.write(words);
                if let Some(cols) = cols {
                    words.flag("--cols", cols);
                }
                if let Some(wrap) = wrap {
                    words.flag("--wrap", wrap);
                }
                words.flag("--out", path_word(out)?);
            }
            MsaCmd::Rename {
                rows,
                to,
                regex,
                replace,
                tree,
            } => {
                rows.write(words);
                if let Some(to) = to {
                    words.flag("--to", to);
                }
                if let Some(regex) = regex {
                    words.flag("--regex", regex);
                }
                if let Some(replace) = replace {
                    words.flag("--replace", replace);
                }
                if *tree {
                    words.switch("--tree");
                }
            }
        }
        Ok(())
    }
}
