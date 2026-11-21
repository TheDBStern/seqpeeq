//! Help for each command, in the README's words, for `:CMD -h` in the
//! interface and `seqpeeq run "CMD -h"`.
//!
//! The text is copied from README.md without its Markdown; a test keeps
//! every syntax line and description the same as the README's.

/// One command's help.
#[derive(Debug)]
pub struct Entry {
    /// The command's words, as typed: `msa fill`, `open`.
    pub path: &'static str,
    /// How it is written, as in the README's table.
    pub syntax: &'static str,
    /// What it does.
    pub about: &'static str,
    /// What its placeholders mean, and anything else the README says of it.
    pub notes: &'static [&'static str],
}

/// A help page: a title and lines, not yet wrapped to a width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpPage {
    pub title: String,
    pub lines: Vec<String>,
}

const ROWS: &str = "Rows are numbered from 1, e.g. --rows 3-7,12; or chosen with --name NAME (exact; \
                    repeatable) or --match REGEX (searched for in names).";
const COLS: &str = "Columns are numbered from 1, e.g. --cols 100-250.";
const ROW_COL: &str = "Rows and columns are numbered from 1.";
const REGION: &str = "A region is contig:start-end, or a contig's name for all of it.";
const CLADE: &str = "A clade is --tip NAME, or --mrca A --mrca B (the most recent common ancestor of \
                     two tips), or --node N (the Nth node in pre-order).";
const REGEX: &str = "The pattern is a Rust regular expression \
                     (docs.rs/regex/latest/regex/#syntax), and every match in a name is replaced.";
const CASE: &str =
    "Patterns are case-sensitive, unlike / search. Start one with (?i) to ignore case.";
const GROUPS: &str = "In --replace, $1 or ${1} is the first group, ${name} a named group and $$ a \
                      dollar sign. Write ${2}_, not $2_, when a group is followed by a letter, digit \
                      or _: seqpeeq refuses $2_ rather than read it as a group called 2_.";
const QUOTES: &str = "Quote patterns with single quotes. In a shell, as in seqpeeq run, put the whole \
                      command in double quotes and write each $ as \\$.";
const CLASH: &str = "A rename that would give two tips the same name is refused, as is one that \
                     would give two linked sequences the same name. An alignment on its own may end \
                     up with duplicate names; seqpeeq says how many.";

/// Every command, in the README's order: files, alignments, trees, BAMs.
pub const ENTRIES: &[Entry] = &[
    Entry {
        path: "open",
        syntax: "open PATH [--kind msa|tree|bam]",
        about: "Open a file. The file type is inferred from the extension unless given.",
        notes: &[],
    },
    Entry {
        path: "use",
        syntax: "use N",
        about: "Make the Nth open file (counting from 1) the one commands apply to.",
        notes: &[],
    },
    Entry {
        path: "close",
        syntax: "close",
        about: "Close the current file. Nothing is written.",
        notes: &[],
    },
    Entry {
        path: "undo",
        syntax: "undo",
        about: "Take back the last change to the current file.",
        notes: &[],
    },
    Entry {
        path: "redo",
        syntax: "redo",
        about: "Reapply the change that was last undone.",
        notes: &[],
    },
    Entry {
        path: "msa delete-rows",
        syntax: "msa delete-rows --rows … | --name … | --match …",
        about: "Remove sequences.",
        notes: &[ROWS],
    },
    Entry {
        path: "msa delete-cols",
        syntax: "msa delete-cols --cols COLS",
        about: "Remove columns.",
        notes: &[COLS],
    },
    Entry {
        path: "msa fill",
        syntax: "msa fill [rows] [--cols COLS] --with CHAR",
        about: "Overwrite a block with one character, such as - or N.",
        notes: &[ROWS, COLS],
    },
    Entry {
        path: "msa overwrite",
        syntax: "msa overwrite --row ROW --col COL --seq SEQ",
        about: "Replace residues in one row, from a column on, with the characters given.",
        notes: &[ROW_COL],
    },
    Entry {
        path: "msa insert",
        syntax: "msa insert --row ROW --col COL --seq SEQ",
        about: "Insert characters into one row at a column. The residues after it move right as \
                far as the gaps it uses up. Gap columns are added at the end when the row has too \
                few.",
        notes: &[ROW_COL],
    },
    Entry {
        path: "msa remove",
        syntax: "msa remove --row ROW --col COL --count N",
        about: "Remove characters from one row at a column. The residues after them move left as \
                far as the next gap, leaving gaps behind.",
        notes: &[ROW_COL],
    },
    Entry {
        path: "msa extract",
        syntax: "msa extract [rows] [--cols COLS] [--wrap N] --out FILE",
        about: "Write sequences, or parts of them, to a new FASTA file. --wrap sets residues per \
                line; the default is one line per sequence.",
        notes: &[ROWS, COLS],
    },
    Entry {
        path: "msa rename",
        syntax: "msa rename [rows] --to NAME | --regex RE --replace TEXT [--tree]",
        about: "Rename one sequence with --to, or many with --regex and --replace. --tree also \
                renames the matching tips in the linked tree.",
        notes: &[
            ROWS,
            "Example: msa rename --regex '^([^_]+)_(.+)$' --replace '${2}_$1' turns \
             Wuhan_2019.2 into 2019.2_Wuhan.",
            "Example: msa rename --match Delta --regex '^' --replace 'VOC_' adds VOC_ only to \
             rows whose names contain Delta.",
            REGEX,
            CASE,
            GROUPS,
            QUOTES,
            CLASH,
        ],
    },
    Entry {
        path: "tree extract",
        syntax: "tree extract [clade] --out FILE [--seqs FILE] [--wrap N]",
        about: "Write a clade, or the whole tree with no clade given, to a new file. With --seqs, \
                also write its tips' sequences from the linked alignment.",
        notes: &[CLADE, "--wrap sets residues per line in the --seqs file."],
    },
    Entry {
        path: "tree prune",
        syntax: "tree prune clade [--seqs]",
        about: "Remove a clade. --seqs also deletes its sequences from the linked alignment.",
        notes: &[CLADE],
    },
    Entry {
        path: "tree reroot",
        syntax: "tree reroot clade | --outgroup TIP … | --midpoint",
        about: "Root on the branch above a node, between an outgroup and the rest, or at the \
                midpoint of the longest path between tips.",
        notes: &[CLADE, "--outgroup may be given more than once."],
    },
    Entry {
        path: "tree ladderize",
        syntax: "tree ladderize [--reverse]",
        about: "Order every node's children by tip count, fewest first (--reverse: most first).",
        notes: &[],
    },
    Entry {
        path: "tree rename",
        syntax: "tree rename [clade] --to NAME | --regex RE --replace TEXT [--seqs]",
        about: "Rename one tip with --to, or many with --regex and --replace. --seqs also renames \
                the matching sequences in the linked alignment.",
        notes: &[
            CLADE,
            "--to renames the tip given; --regex renames across the whole tree.",
            "Example: tree rename --regex '_' --replace '-' --seqs turns Wuhan_2019.2 into \
             Wuhan-2019.2, in the tree and in its linked alignment.",
            REGEX,
            CASE,
            GROUPS,
            QUOTES,
            CLASH,
        ],
    },
    Entry {
        path: "tree link",
        syntax: "tree link FILE",
        about: "Pair the tree with an open alignment, matching tip and sequence names. FILE may be \
                the open alignment's path or just its file name.",
        notes: &["In the interface, Tab completes the file name."],
    },
    Entry {
        path: "tree unlink",
        syntax: "tree unlink",
        about: "Drop the tree's link to an alignment.",
        notes: &[],
    },
    Entry {
        path: "bam ref",
        syntax: "bam ref FILE",
        about: "Attach a reference FASTA to the current BAM, or to every BAM in its stack, for \
                reference bases, mismatches and export-ref.",
        notes: &[],
    },
    Entry {
        path: "bam export-region",
        syntax: "bam export-region REGION --out FILE",
        about: "Write every record overlapping a region to a new BAM, with its index.",
        notes: &[REGION],
    },
    Entry {
        path: "bam export-reads",
        syntax: "bam export-reads [--region REGION] [--name NAME … | --names-file FILE] \
                 [--with-mates] [--clip] [--everywhere] --out FILE",
        about: "Write reads to a new BAM, FASTA or FASTQ, chosen by --out's extension.",
        notes: &[
            REGION,
            "--name may be given more than once; --names-file reads one name per line.",
            "--with-mates adds each read's mate and the other parts of split reads.",
            "--clip keeps only the bases inside the region (FASTA or FASTQ).",
            "--everywhere searches the whole BAM for the names instead of reading a region.",
        ],
    },
    Entry {
        path: "bam export-ref",
        syntax: "bam export-ref REGION --out FILE",
        about: "Write a stretch of the reference to a new FASTA.",
        notes: &[REGION],
    },
    Entry {
        path: "bam stack",
        syntax: "bam stack FILE",
        about: "Stack the current BAM with an open BAM that has the same contigs.",
        notes: &[],
    },
    Entry {
        path: "bam unstack",
        syntax: "bam unstack",
        about: "Split the current BAM's stack back into tabs.",
        notes: &[],
    },
];

/// The groups of commands, headed as in the README.
const HEADINGS: &[(&str, &str)] = &[
    ("", "Files"),
    ("msa", "Alignments"),
    ("tree", "Trees"),
    ("bam", "BAMs"),
];

/// The group a command's words belong to: `msa`, `tree`, `bam`, or `""`.
fn group_of(path: &str) -> &str {
    path.split_once(' ').map_or("", |(group, _)| group)
}

/// The help a typed line asks for: `None` when it is not a help request
/// (no `-h` or `--help` word, and not `help` alone); else the page, or why
/// there is none.
pub fn help_request(line: &str) -> Option<Result<HelpPage, String>> {
    let words = shlex::split(line)?;
    // `help msa fill` asks what `msa fill -h` does.
    let leading_help = words.first().is_some_and(|w| w == "help");
    let rest = if leading_help {
        &words[1..]
    } else {
        &words[..]
    };
    // The command's words come before its first flag: a group's name and
    // its command, or a command of its own.
    let mut names = rest
        .iter()
        .map(String::as_str)
        .take_while(|w| !w.starts_with('-'));
    let first = names.next();
    let second = first
        .filter(|f| matches!(*f, "msa" | "tree" | "bam"))
        .and_then(|_| names.next());
    let parser = <crate::Command as clap::CommandFactory>::command();
    let command = match (first, second) {
        (Some(group), Some(sub)) => parser
            .find_subcommand(group)
            .and_then(|g| g.find_subcommand(sub)),
        (Some(name), None) => parser.find_subcommand(name),
        (None, _) => None,
    };
    // A -h after `--`, or as the value of a flag (`--name -h`), is not a
    // request for help: it is a name to act on.
    let asked = leading_help
        || rest
            .iter()
            .enumerate()
            .take_while(|(_, w)| *w != "--")
            .any(|(i, w)| {
                (w == "-h" || w == "--help") && !(i > 0 && takes_value(command, &rest[i - 1]))
            });
    if !asked {
        return None;
    }
    let page = match (first, second) {
        (None | Some("help"), _) => Ok(all()),
        (Some(group @ ("msa" | "tree" | "bam")), None) => Ok(group_page(group)),
        (Some(group), Some(sub)) => {
            // The parser knows each command's aliases (`save` for
            // `extract`); ask it for the command's own name.
            let name = command.map_or(sub, |c| c.get_name());
            entry_page(&format!("{group} {name}")).ok_or(format!(
                "no command '{group} {sub}'; :{group} -h lists them"
            ))
        }
        (Some(name), None) => {
            entry_page(name).ok_or(format!("no command '{name}'; :help lists them"))
        }
    };
    Some(page)
}

/// Whether `word` is a flag of `command` that takes a value, so that the
/// word after it is that value.
fn takes_value(command: Option<&clap::Command>, word: &str) -> bool {
    let Some(command) = command else {
        return false;
    };
    let mut arguments = command.get_arguments();
    let arg = if let Some(long) = word.strip_prefix("--") {
        // `--flag=value` holds its value already.
        (!long.contains('=')).then(|| arguments.find(|a| a.get_long() == Some(long)))
    } else if let Some(short) = word.strip_prefix('-') {
        let mut letters = short.chars();
        match (letters.next(), letters.next()) {
            (Some(letter), None) => Some(arguments.find(|a| a.get_short() == Some(letter))),
            _ => None,
        }
    } else {
        None
    };
    arg.flatten().is_some_and(|a| a.get_action().takes_values())
}

fn entry_page(path: &str) -> Option<HelpPage> {
    let entry = ENTRIES.iter().find(|e| e.path == path)?;
    let mut lines = vec![
        entry.syntax.to_string(),
        String::new(),
        entry.about.to_string(),
    ];
    if !entry.notes.is_empty() {
        lines.push(String::new());
        lines.extend(entry.notes.iter().map(|note| format!("- {note}")));
    }
    Some(HelpPage {
        title: format!("help: {}", entry.path),
        lines,
    })
}

fn syntaxes(group: &str) -> impl Iterator<Item = String> + '_ {
    ENTRIES
        .iter()
        .filter(move |e| group_of(e.path) == group)
        .map(|e| e.syntax.to_string())
}

fn group_page(group: &str) -> HelpPage {
    let mut lines: Vec<String> = syntaxes(group).collect();
    lines.push(String::new());
    lines.push(format!(":{group} COMMAND -h for details"));
    HelpPage {
        title: format!("help: {group} commands"),
        lines,
    }
}

fn all() -> HelpPage {
    let mut lines = Vec::new();
    for (group, heading) in HEADINGS {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!("{heading}:"));
        lines.extend(syntaxes(group));
    }
    lines.push(String::new());
    lines.push(":COMMAND -h for details".to_string());
    HelpPage {
        title: "help: commands".to_string(),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes_of(path: &str) -> &'static [&'static str] {
        ENTRIES
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("no entry {path}"))
            .notes
    }

    #[test]
    fn a_command_with_h_shows_its_entry() {
        let page = help_request("msa fill -h").unwrap().unwrap();
        assert_eq!(page.title, "help: msa fill");
        assert_eq!(page.lines[0], "msa fill [rows] [--cols COLS] --with CHAR");
        assert!(
            page.lines
                .iter()
                .any(|l| l.contains("Overwrite a block with one character"))
        );
        assert!(page.lines.iter().any(|l| l.contains("--rows 3-7,12")));
    }

    #[test]
    fn help_after_flags_and_values_still_finds_the_command() {
        let page = help_request("msa fill --rows 1 --with N --help")
            .unwrap()
            .unwrap();
        assert_eq!(page.title, "help: msa fill");
        let page = help_request("tree link a.fa -h").unwrap().unwrap();
        assert_eq!(page.title, "help: tree link");
    }

    #[test]
    fn an_alias_shows_its_commands_help() {
        let page = help_request("msa save -h").unwrap().unwrap();
        assert_eq!(page.title, "help: msa extract");
        let page = help_request("tree save -h").unwrap().unwrap();
        assert_eq!(page.title, "help: tree extract");
    }

    #[test]
    fn a_value_attached_with_equals_is_not_a_help_request() {
        assert!(help_request("msa rename --regex x --replace=-h").is_none());
        assert!(help_request("msa fill --with N").is_none());
        assert!(help_request("msa fill 'unbalanced -h").is_none());
    }

    #[test]
    fn help_before_a_command_shows_its_help() {
        let title = |line: &str| help_request(line).unwrap().unwrap().title;
        assert_eq!(title("help msa fill"), "help: msa fill");
        assert_eq!(title("help msa"), "help: msa commands");
        assert_eq!(title("help open"), "help: open");
        assert_eq!(title("help tree save"), "help: tree extract");
        assert_eq!(
            help_request("help foo").unwrap().unwrap_err(),
            "no command 'foo'; :help lists them"
        );
    }

    #[test]
    fn h_as_the_value_of_a_flag_is_not_a_help_request() {
        for line in [
            "msa rename --name -h --to x",
            "msa delete-rows --match -h",
            "msa rename --regex -h --replace x",
            "tree prune --tip -h",
            "msa extract -o -h",
            "open -- -h",
        ] {
            assert!(help_request(line).is_none(), "{line}");
        }
        // A switch takes no value, so -h after it still asks for help.
        let page = help_request("msa rename --tree -h").unwrap().unwrap();
        assert_eq!(page.title, "help: msa rename");
    }

    #[test]
    fn a_group_lists_its_commands() {
        let page = help_request("bam -h").unwrap().unwrap();
        assert_eq!(page.title, "help: bam commands");
        for entry in ENTRIES.iter().filter(|e| e.path.starts_with("bam ")) {
            assert!(
                page.lines.contains(&entry.syntax.to_string()),
                "{}",
                entry.path
            );
        }
        assert_eq!(page.lines.last().unwrap(), ":bam COMMAND -h for details");
    }

    #[test]
    fn help_alone_lists_every_command() {
        for line in ["help", "-h", "--help"] {
            let page = help_request(line).unwrap().unwrap();
            assert_eq!(page.title, "help: commands");
            for entry in ENTRIES {
                assert!(
                    page.lines.contains(&entry.syntax.to_string()),
                    "{}",
                    entry.path
                );
            }
        }
    }

    #[test]
    fn an_unknown_command_says_where_to_look() {
        assert_eq!(
            help_request("msa foo -h").unwrap().unwrap_err(),
            "no command 'msa foo'; :msa -h lists them"
        );
        assert_eq!(
            help_request("foo -h").unwrap().unwrap_err(),
            "no command 'foo'; :help lists them"
        );
    }

    #[test]
    fn every_command_the_parser_knows_has_one_entry() {
        fn walk(prefix: &str, cmd: &clap::Command, out: &mut Vec<String>) {
            for sub in cmd.get_subcommands() {
                let path = format!("{prefix}{}", sub.get_name());
                if sub.has_subcommands() {
                    walk(&format!("{path} "), sub, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut known = Vec::new();
        walk(
            "",
            &<crate::Command as clap::CommandFactory>::command(),
            &mut known,
        );
        known.sort();
        let mut entries: Vec<String> = ENTRIES.iter().map(|e| e.path.to_string()).collect();
        entries.sort();
        assert_eq!(entries, known);
    }

    #[test]
    fn every_entry_matches_the_readme() {
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
                .expect("README.md at the repo root")
                .replace("\\|", "|")
                .replace('`', "");
        for entry in ENTRIES {
            assert!(
                readme.contains(entry.syntax),
                "README lacks {:?}",
                entry.syntax
            );
            assert!(
                readme.contains(entry.about),
                "README lacks {:?}",
                entry.about
            );
        }
    }

    #[test]
    fn notes_name_what_their_placeholders_mean() {
        let has = |path: &str, text: &str| notes_of(path).iter().any(|n| n.contains(text));
        assert!(has("msa delete-cols", "100-250"));
        assert!(has("bam export-region", "contig:start-end"));
        assert!(has("tree prune", "--mrca"));
        assert!(has("bam export-reads", "--with-mates"));
        assert!(has("msa rename", "(?i)"));
        assert!(has("tree rename", "${2}_"));
    }
}
