//! The command language: every change to a session is one [`Command`].
//!
//! A command has one canonical text form ([`Command::to_line`]) that
//! [`Command::parse_line`] reads back to an equal command. That text is what
//! the session log stores, what the in-app command line accepts and what
//! `seqpeeq run` takes, so the three can never drift apart.

pub mod bam;
pub mod msa;
pub mod tree;

use std::fmt::Display;
use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};

use crate::{Error, Result};
use bam::BamCmd;
use msa::MsaCmd;
use tree::TreeCmd;

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
            "fa" | "fasta" | "fas" | "fsa" | "mfa" | "fna" | "faa" | "aln" | "afa" => {
                Some(Kind::Msa)
            }
            "nwk" | "newick" | "nw" | "tree" | "tre" | "treefile" | "contree" => Some(Kind::Tree),
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
    /// Make the Nth open file (counting from 1) the one commands apply to.
    Use { dataset: usize },
    /// Close the current file. Nothing is written.
    Close,
    /// Take back the last change to the current file.
    Undo,
    /// Reapply the change that was last undone.
    Redo,
    /// Alignment commands.
    #[command(subcommand)]
    Msa(MsaCmd),
    /// Tree commands.
    #[command(subcommand)]
    Tree(TreeCmd),
    /// BAM commands.
    #[command(subcommand)]
    Bam(BamCmd),
}

/// The words of a command's text form, before quoting.
pub(crate) struct Words {
    words: Vec<String>,
    positionals: Vec<String>,
}

impl Words {
    fn new(name: &str) -> Words {
        Words {
            words: vec![name.to_string()],
            positionals: Vec::new(),
        }
    }

    /// A further command word, such as a subcommand name.
    fn word(&mut self, word: &str) {
        self.words.push(word.to_string());
    }

    /// A flag and its value. A value starting with `-` is attached with `=`,
    /// because on its own it would be read as another flag.
    fn flag(&mut self, flag: &str, value: impl Display) {
        let value = value.to_string();
        if value.starts_with('-') {
            self.words.push(format!("{flag}={value}"));
        } else {
            self.words.push(flag.to_string());
            self.words.push(value);
        }
    }

    /// A flag that takes no value.
    fn switch(&mut self, flag: &str) {
        self.words.push(flag.to_string());
    }

    fn positional(&mut self, value: impl Display) {
        self.positionals.push(value.to_string());
    }
}

impl Command {
    /// Parse one line of command text, as typed or as read from a log.
    pub fn parse_line(line: &str) -> Result<Command> {
        let words =
            shlex::split(line).ok_or_else(|| Error::Parse("unbalanced quotes".to_string()))?;
        if words.is_empty() {
            return Err(Error::Parse("empty command".to_string()));
        }
        Command::try_parse_from(&words).map_err(|error| Error::Parse(describe(&error, &words)))
    }

    /// The canonical text form: command words, then flags, then positionals.
    pub fn to_line(&self) -> Result<String> {
        let words = match self {
            Command::Open { path, kind } => {
                let mut words = Words::new("open");
                if let Some(kind) = kind {
                    words.flag("--kind", kind.as_str());
                }
                words.positional(path_word(path)?);
                words
            }
            Command::Use { dataset } => {
                let mut words = Words::new("use");
                words.positional(dataset);
                words
            }
            Command::Close => Words::new("close"),
            Command::Undo => Words::new("undo"),
            Command::Redo => Words::new("redo"),
            Command::Msa(cmd) => {
                let mut words = Words::new("msa");
                cmd.write(&mut words)?;
                words
            }
            Command::Tree(cmd) => {
                let mut words = Words::new("tree");
                cmd.write(&mut words)?;
                words
            }
            Command::Bam(cmd) => {
                let mut words = Words::new("bam");
                cmd.write(&mut words)?;
                words
            }
        };
        let Words {
            mut words,
            positionals,
        } = words;
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
            let quoted = quote(word)?;
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&quoted);
        }
        Ok(line)
    }
}

/// A parse error as one line that says what to change. clap's own first line
/// often stops short of that ("the following required arguments were not
/// provided:"), and the in-app command line has room for one line only.
fn describe(error: &clap::Error, words: &[String]) -> String {
    use clap::CommandFactory;
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    match error.kind() {
        ErrorKind::MissingRequiredArgument => {
            if let Some(ContextValue::Strings(missing)) = error.get(ContextKind::InvalidArg) {
                return format!("missing {}", missing.join(", "));
            }
        }
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let all = Command::command();
            if let Some(group) = all.find_subcommand(&words[0]) {
                let names: Vec<&str> = group.get_subcommands().map(|sub| sub.get_name()).collect();
                return format!("{} needs one of: {}", words[0], names.join(", "));
            }
        }
        _ => {}
    }
    let text = error.render().to_string();
    let first = text.lines().next().unwrap_or("invalid command");
    first.strip_prefix("error: ").unwrap_or(first).to_string()
}

/// Quote a word for the text form only when it needs it. shlex alone would
/// also quote `,` and `=`, turning `--rows 3-7,12` into `--rows '3-7,12'`;
/// neither character means anything to the parser.
fn quote(word: &str) -> Result<std::borrow::Cow<'_, str>> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./:,=+@%".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        return Ok(word.into());
    }
    shlex::try_quote(word).map_err(|_| Error::Unloggable)
}

fn path_word(path: &Path) -> Result<String> {
    path.to_str().map(str::to_string).ok_or(Error::Unloggable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::command::bam::BamCmd;
    use crate::command::msa::{MsaCmd, RowSel};
    use crate::command::tree::{NodeSel, TreeCmd};
    use crate::ranges::Ranges;
    use proptest::prelude::*;
    use std::path::Path;

    #[test]
    fn kind_from_extension() {
        assert_eq!(Kind::from_path(Path::new("a.FASTA")), Some(Kind::Msa));
        assert_eq!(Kind::from_path(Path::new("a.aln.gz")), Some(Kind::Msa));
        assert_eq!(Kind::from_path(Path::new("x.treefile")), Some(Kind::Tree));
        assert_eq!(Kind::from_path(Path::new("r.bam")), Some(Kind::Bam));
        for name in ["a.fas", "a.fsa", "a.mfa", "a.FAS.gz"] {
            assert_eq!(Kind::from_path(Path::new(name)), Some(Kind::Msa), "{name}");
        }
        for name in ["x.contree", "x.nw"] {
            assert_eq!(Kind::from_path(Path::new(name)), Some(Kind::Tree), "{name}");
        }
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

    const CANONICAL: &[&str] = &[
        "use 2",
        "close",
        "undo",
        "redo",
        "msa delete-rows --rows 3-7,12",
        "msa delete-rows --name A --name 'B c'",
        "msa delete-rows --match '^Homo_'",
        "msa delete-cols --cols 100-250",
        "msa fill --rows 1-3 --cols 10-20 --with N",
        "msa fill --cols 5 --with=-",
        "msa extract --rows 1-10 --cols 5-90 --wrap 60 --out sub.fa",
        "msa extract --out all.fa",
        "msa rename --rows 5 --to 'Homo sapiens'",
        "msa rename --regex _ --replace ' '",
        "msa rename --regex=-x --replace ''",
        "msa overwrite --row 3 --col 27 --seq ACG-T",
        "msa overwrite --row 1 --col 1 --seq=-A",
        "msa insert --row 3 --col 27 --seq AC",
        "msa insert --row 1 --col 1 --seq=-",
        "msa remove --row 3 --col 27 --count 2",
        "tree extract --mrca A --mrca B --out sub.nwk --seqs sub.fa --wrap 60",
        "tree extract --out all.nwk",
        "tree prune --tip 'Homo sapiens (human)'",
        "tree prune --node 12",
        "tree reroot --outgroup A --outgroup 'x, y'",
        "tree reroot --midpoint",
        "tree reroot --tip=-odd",
        "tree ladderize",
        "tree ladderize --reverse",
        "tree rename --tip A --to 'New name'",
        "tree rename --node 3 --to x",
        "tree rename --regex _ --replace ' '",
        "tree link aln.fa",
        "tree link -- -odd.fa",
        "tree unlink",
    ];

    #[test]
    fn tree_commands_are_typed_in_several_ways_but_logged_in_one() {
        for (typed, logged) in [
            ("tree save -o t.nwk", "tree extract --out t.nwk"),
            (
                "tree extract -o t.nwk --tip A",
                "tree extract --tip A --out t.nwk",
            ),
            (
                "tree reroot --midpoint --outgroup B",
                "tree reroot --outgroup B --midpoint",
            ),
        ] {
            assert_eq!(
                Command::parse_line(typed).unwrap().to_line().unwrap(),
                logged
            );
        }
        assert_eq!(
            Command::parse_line("tree").unwrap_err().to_string(),
            "tree needs one of: extract, prune, reroot, ladderize, rename, link, unlink"
        );
        for bad in [
            "tree prune --tip A --node 2",
            "tree prune --mrca A --tip B",
            "tree link",
        ] {
            assert!(Command::parse_line(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn canonical_lines_round_trip() {
        for line in CANONICAL {
            assert_eq!(Command::parse_line(line).unwrap().to_line().unwrap(), *line);
        }
    }

    #[test]
    fn accepted_spellings_normalise() {
        for (typed, canonical) in [
            ("msa extract -o x.fa", "msa extract --out x.fa"),
            ("msa save -o x.fa", "msa extract --out x.fa"),
            ("msa fill --with - --cols 5", "msa fill --cols 5 --with=-"),
            (
                "msa delete-rows --rows 12,3-7",
                "msa delete-rows --rows 3-7,12",
            ),
        ] {
            assert_eq!(
                Command::parse_line(typed).unwrap().to_line().unwrap(),
                canonical
            );
        }
    }

    #[test]
    fn conflicting_flags_are_refused() {
        for line in [
            "msa delete-rows --rows 1 --name A",
            "msa delete-rows --rows 1 --match A",
            "msa rename --to X --regex a --replace b",
            "msa rename --regex a",
            "msa rename --replace b",
            "msa delete-cols",
            "msa fill --cols 1 --with NN",
            "msa delete-cols --cols 0",
        ] {
            assert!(
                matches!(Command::parse_line(line), Err(Error::Parse(_))),
                "{line}"
            );
        }
    }

    #[test]
    fn range_errors_are_one_line_and_say_why() {
        let Err(Error::Parse(m)) = Command::parse_line("msa delete-cols --cols 7-3") else {
            panic!()
        };
        assert!(
            m.ends_with("range 7-3 runs backwards") && !m.contains('\n'),
            "{m}"
        );
    }

    #[test]
    fn subcommands_have_no_help_either() {
        for bad in ["msa", "msa --help", "msa help", "msa delete-cols --help"] {
            let Err(Error::Parse(m)) = Command::parse_line(bad) else {
                panic!("{bad}")
            };
            assert!(!m.contains('\n') && !m.is_empty(), "{bad}: {m:?}");
        }
    }

    /// Text with the characters most likely to break quoting or flag parsing.
    fn text() -> impl Strategy<Value = String> {
        prop_oneof!["[ '\"\\-=$\\\\a-zé]{0,10}", "[^\\x00\\r\\n]{0,12}"]
    }

    fn path() -> impl Strategy<Value = PathBuf> {
        prop_oneof!["[ '\"\\-=$\\\\a-zé]{1,10}", "[^\\x00\\r\\n]{1,40}"].prop_map(PathBuf::from)
    }

    fn ranges() -> impl Strategy<Value = Ranges> {
        proptest::collection::vec((0usize..500, 1usize..50), 1..4).prop_map(|v| {
            Ranges::new(v.into_iter().map(|(start, len)| start..start + len)).unwrap()
        })
    }

    fn row_sel() -> impl Strategy<Value = RowSel> {
        prop_oneof![
            Just(RowSel::default()),
            ranges().prop_map(|rows| RowSel {
                rows: Some(rows),
                ..RowSel::default()
            }),
            proptest::collection::vec(text(), 1..3).prop_map(|name| RowSel {
                name,
                ..RowSel::default()
            }),
            text().prop_map(|matching| RowSel {
                matching: Some(matching),
                ..RowSel::default()
            }),
        ]
    }

    fn msa_cmd() -> impl Strategy<Value = MsaCmd> {
        let cols = || proptest::option::of(ranges());
        let fill_char = any::<char>().prop_filter("loggable", |c| !['\n', '\r', '\0'].contains(c));
        prop_oneof![
            row_sel().prop_map(|rows| MsaCmd::DeleteRows { rows }),
            ranges().prop_map(|cols| MsaCmd::DeleteCols { cols }),
            (row_sel(), cols(), fill_char).prop_map(|(rows, cols, with)| MsaCmd::Fill {
                rows,
                cols,
                with
            }),
            (
                row_sel(),
                cols(),
                proptest::option::of(any::<usize>()),
                path()
            )
                .prop_map(|(rows, cols, wrap, out)| MsaCmd::Extract {
                    rows,
                    cols,
                    wrap,
                    out
                }),
            (row_sel(), proptest::option::of(text()), any::<bool>()).prop_map(
                |(rows, to, tree)| MsaCmd::Rename {
                    rows,
                    to,
                    regex: None,
                    replace: None,
                    tree
                }
            ),
            (row_sel(), text(), text(), any::<bool>()).prop_map(|(rows, regex, replace, tree)| {
                MsaCmd::Rename {
                    rows,
                    to: None,
                    regex: Some(regex),
                    replace: Some(replace),
                    tree,
                }
            }),
            (1..10_000usize, 1..10_000usize, "[!-=?-~]{1,12}")
                .prop_map(|(row, col, seq)| MsaCmd::Overwrite { row, col, seq }),
            (1..10_000usize, 1..10_000usize, "[!-=?-~]{1,12}")
                .prop_map(|(row, col, seq)| MsaCmd::Insert { row, col, seq }),
            (1..10_000usize, 1..10_000usize, 1..10_000usize)
                .prop_map(|(row, col, count)| MsaCmd::Remove { row, col, count }),
        ]
    }

    fn command() -> impl Strategy<Value = Command> {
        let kind = proptest::option::of(prop_oneof![
            Just(Kind::Msa),
            Just(Kind::Tree),
            Just(Kind::Bam)
        ]);
        prop_oneof![
            (path(), kind).prop_map(|(path, kind)| Command::Open { path, kind }),
            any::<usize>().prop_map(|dataset| Command::Use { dataset }),
            Just(Command::Undo),
            Just(Command::Redo),
            Just(Command::Close),
            msa_cmd().prop_map(Command::Msa),
            tree_cmd().prop_map(Command::Tree),
            bam_cmd().prop_map(Command::Bam),
        ]
    }

    fn bam_cmd() -> impl Strategy<Value = BamCmd> {
        let region = || "[^\\x00\\r\\n]{1,20}";
        prop_oneof![
            path().prop_map(|file| BamCmd::Ref { file }),
            (region(), path()).prop_map(|(region, out)| BamCmd::ExportRegion { region, out }),
            (region(), path()).prop_map(|(region, out)| BamCmd::ExportRef { region, out }),
            (
                region(),
                proptest::collection::vec(text().prop_filter("a name", |t| !t.is_empty()), 0..3),
                proptest::option::of(path()),
                path(),
                any::<bool>(),
                any::<bool>(),
            )
                .prop_map(|(region, names, file, out, with_mates, clip)| {
                    // --names-file and --name are never given together.
                    let names_file = if names.is_empty() { file } else { None };
                    BamCmd::ExportReads {
                        region,
                        names,
                        names_file,
                        out,
                        with_mates,
                        clip,
                    }
                }),
        ]
    }

    fn node_sel() -> impl Strategy<Value = NodeSel> {
        prop_oneof![
            Just(NodeSel::default()),
            text().prop_map(|tip| NodeSel {
                tip: Some(tip),
                ..NodeSel::default()
            }),
            (text(), text()).prop_map(|(a, b)| NodeSel {
                mrca: vec![a, b],
                ..NodeSel::default()
            }),
            any::<usize>().prop_map(|node| NodeSel {
                node: Some(node),
                ..NodeSel::default()
            }),
        ]
    }

    fn tree_cmd() -> impl Strategy<Value = TreeCmd> {
        prop_oneof![
            (
                node_sel(),
                path(),
                proptest::option::of(path()),
                proptest::option::of(any::<usize>())
            )
                .prop_map(|(at, out, seqs, wrap)| TreeCmd::Extract {
                    at,
                    out,
                    seqs,
                    wrap
                }),
            (node_sel(), any::<bool>()).prop_map(|(at, seqs)| TreeCmd::Prune { at, seqs }),
            (
                node_sel(),
                proptest::collection::vec(text(), 0..3),
                any::<bool>()
            )
                .prop_map(|(at, outgroup, midpoint)| TreeCmd::Reroot {
                    at,
                    outgroup,
                    midpoint
                }),
            any::<bool>().prop_map(|reverse| TreeCmd::Ladderize { reverse }),
            (node_sel(), proptest::option::of(text()), any::<bool>()).prop_map(|(at, to, seqs)| {
                TreeCmd::Rename {
                    at,
                    to,
                    regex: None,
                    replace: None,
                    seqs,
                }
            }),
            (text(), text(), any::<bool>()).prop_map(|(regex, replace, seqs)| TreeCmd::Rename {
                at: NodeSel::default(),
                to: None,
                regex: Some(regex),
                replace: Some(replace),
                seqs
            }),
            path().prop_map(|file| TreeCmd::Link { file }),
            Just(TreeCmd::Unlink),
        ]
    }

    proptest! {
        #[test]
        fn round_trip(cmd in command()) {
            let line = cmd.to_line().unwrap();
            prop_assert_eq!(Command::parse_line(&line).unwrap(), cmd, "{}", line);
        }
    }

    #[test]
    fn parse_errors_say_what_is_missing() {
        for (line, message) in [
            ("open", "missing <PATH>"),
            ("msa fill --cols 1", "missing --with <WITH>"),
            ("msa extract", "missing --out <OUT>"),
            ("msa delete-cols", "missing --cols <COLS>"),
            ("msa rename --regex a", "missing --replace <REPLACE>"),
            (
                "msa",
                "msa needs one of: delete-rows, delete-cols, fill, overwrite, insert, remove, extract, rename",
            ),
        ] {
            let Err(Error::Parse(m)) = Command::parse_line(line) else {
                panic!("{line}")
            };
            assert_eq!(m, message, "{line}");
        }
    }
}
