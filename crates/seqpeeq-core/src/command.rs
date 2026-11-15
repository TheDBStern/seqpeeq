//! The command language: every change to a session is one [`Command`].
//!
//! A command has one canonical text form ([`Command::to_line`]) that
//! [`Command::parse_line`] reads back to an equal command. That text is what
//! the session log stores, what the in-app command line accepts and what
//! `seqpeeq run` takes, so the three can never drift apart.

use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};

use crate::{Error, Result};

/// What a file holds, and therefore which viewer and commands apply to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Msa,
    Tree,
    Bam,
}

impl Kind {
    /// Guess the kind from the file extension, ignoring case and one `.gz`.
    pub fn from_path(path: &Path) -> Option<Kind> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        let name = name.strip_suffix(".gz").unwrap_or(&name);
        match name.rsplit_once('.')?.1 {
            "fa" | "fasta" | "fna" | "faa" | "aln" | "afa" => Some(Kind::Msa),
            "nwk" | "newick" | "tree" | "tre" | "treefile" => Some(Kind::Tree),
            "bam" => Some(Kind::Bam),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Msa => "msa",
            Kind::Tree => "tree",
            Kind::Bam => "bam",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Parser)]
#[command(
    no_binary_name = true,
    disable_help_flag = true,
    disable_help_subcommand = true
)]
pub enum Command {
    /// Open a file. The kind is taken from the extension unless given.
    Open {
        path: PathBuf,
        #[arg(long)]
        kind: Option<Kind>,
    },
}

impl Command {
    /// Parse one line of command text, as typed or as read from a log.
    pub fn parse_line(line: &str) -> Result<Command> {
        let words =
            shlex::split(line).ok_or_else(|| Error::Parse("unbalanced quotes".to_string()))?;
        if words.is_empty() {
            return Err(Error::Parse("empty command".to_string()));
        }
        Command::try_parse_from(words).map_err(|e| {
            let text = e.render().to_string();
            let first = text.lines().next().unwrap_or("invalid command");
            Error::Parse(first.strip_prefix("error: ").unwrap_or(first).to_string())
        })
    }

    /// The canonical text form: name, then flags, then positionals.
    pub fn to_line(&self) -> Result<String> {
        let (name, flags, positionals) = match self {
            Command::Open { path, kind } => {
                let mut flags = Vec::new();
                if let Some(kind) = kind {
                    flags.push("--kind".to_string());
                    flags.push(kind.as_str().to_string());
                }
                ("open", flags, vec![path_word(path)?])
            }
        };
        let mut words = vec![name.to_string()];
        words.extend(flags);
        // A positional starting with `-` would otherwise be read as a flag.
        if positionals.iter().any(|p| p.starts_with('-')) {
            words.push("--".to_string());
        }
        words.extend(positionals);

        let mut line = String::new();
        for word in &words {
            if word.contains(['\n', '\r', '\0']) {
                return Err(Error::Unloggable);
            }
            let quoted = shlex::try_quote(word).map_err(|_| Error::Unloggable)?;
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&quoted);
        }
        Ok(line)
    }
}

fn path_word(path: &Path) -> Result<String> {
    path.to_str().map(str::to_string).ok_or(Error::Unloggable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use proptest::prelude::*;
    use std::path::Path;

    #[test]
    fn kind_from_extension() {
        assert_eq!(Kind::from_path(Path::new("a.FASTA")), Some(Kind::Msa));
        assert_eq!(Kind::from_path(Path::new("a.aln.gz")), Some(Kind::Msa));
        assert_eq!(Kind::from_path(Path::new("x.treefile")), Some(Kind::Tree));
        assert_eq!(Kind::from_path(Path::new("r.bam")), Some(Kind::Bam));
        assert_eq!(Kind::from_path(Path::new("notes.txt")), None);
    }

    #[test]
    fn parses_open() {
        assert_eq!(
            Command::parse_line("open aln.fa").unwrap(),
            Command::Open {
                path: "aln.fa".into(),
                kind: None
            }
        );
        assert_eq!(
            Command::parse_line("open data.txt --kind tree").unwrap(),
            Command::Open {
                path: "data.txt".into(),
                kind: Some(Kind::Tree)
            }
        );
    }

    #[test]
    fn canonical_text() {
        let c = |p: &str, k| Command::Open {
            path: p.into(),
            kind: k,
        };
        assert_eq!(c("aln.fa", None).to_line().unwrap(), "open aln.fa");
        assert_eq!(
            c("data.txt", Some(Kind::Msa)).to_line().unwrap(),
            "open --kind msa data.txt"
        );
        assert_eq!(c("-odd.fa", None).to_line().unwrap(), "open -- -odd.fa");
        assert_eq!(
            c("my file's \"aln\".fa", None)
                .to_line()
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn refuses_unloggable() {
        let c = Command::Open {
            path: "a\nb.fa".into(),
            kind: None,
        };
        assert!(matches!(c.to_line(), Err(Error::Unloggable)));
    }

    #[test]
    fn parse_errors_are_one_line() {
        for bad in ["bogus", "open", "open 'unterminated", ""] {
            let Err(Error::Parse(m)) = Command::parse_line(bad) else {
                panic!("{bad}")
            };
            assert!(
                !m.contains('\n') && !m.starts_with("error:") && !m.is_empty(),
                "{bad}: {m:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn round_trip(
            path in "[^\\x00\\r\\n]{1,40}",
            kind in proptest::option::of(prop_oneof![Just(Kind::Msa), Just(Kind::Tree), Just(Kind::Bam)]),
        ) {
            let cmd = Command::Open { path: path.into(), kind };
            prop_assert_eq!(Command::parse_line(&cmd.to_line().unwrap()).unwrap(), cmd);
        }
    }
}
