//! The `seqpeeq` command: interactive viewer, `replay` and `run`.

mod app;
mod bam_view;
mod elsewhere;
mod line;
mod msa_view;
mod palette;
mod read_axis;
mod term;
mod tree_view;
mod ui;
mod worker;
mod zoom;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use jiff::Zoned;
use seqpeeq_core::{BamCmd, Command, Kind, Session, SessionLog, check_input, help_request, replay};

/// View and edit alignments, trees and mapped reads in the terminal.
#[derive(Parser)]
#[command(name = "seqpeeq", version)]
struct Cli {
    #[command(subcommand)]
    sub: Option<Sub>,

    /// Files to open (FASTA alignment, Newick tree, BAM)
    files: Vec<PathBuf>,

    /// Treat the files as this kind instead of going by their extensions
    #[arg(long, value_enum)]
    kind: Option<Kind>,

    /// A reference FASTA for the BAM files given (bam ref FASTA after each)
    #[arg(long = "ref", value_name = "FASTA")]
    reference: Option<PathBuf>,

    /// Write the session log here instead of seqpeeq-YYYYMMDD-HHMMSS.log
    #[arg(long, global = true, value_name = "PATH")]
    log: Option<PathBuf>,

    /// Do not write a session log
    #[arg(long, conflicts_with = "log")]
    no_log: bool,
}

#[derive(Subcommand)]
enum Sub {
    /// Re-run the commands of a session log without the interface
    Replay { log_file: PathBuf },
    /// Run commands without the interface; each argument is one command
    Run {
        #[arg(required = true, value_name = "COMMAND")]
        commands: Vec<String>,
    },
}

fn main() -> ExitCode {
    // Returning (rather than calling `exit`) lets the session log clean up.
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("seqpeeq: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if cli.sub.is_some() && (!cli.files.is_empty() || cli.kind.is_some() || cli.no_log) {
        bail!("files, --kind and --no-log cannot be combined with run or replay");
    }
    match cli.sub {
        Some(Sub::Replay { log_file }) => {
            let text = std::fs::read_to_string(&log_file)
                .with_context(|| format!("cannot read {}", log_file.display()))?;
            let mut session = Session::new(cli.log.map(SessionLog::new));
            // Names files written beside the log are looked for there.
            let folder = log_file
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            session.set_replay_dir(Some(folder.to_path_buf()));
            let report = replay(&text, &mut session)?;
            for warning in &report.warnings {
                eprintln!("seqpeeq: warning: {warning}");
            }
            let noun = if report.commands == 1 {
                "command"
            } else {
                "commands"
            };
            println!("replayed {} {noun}", report.commands);
        }
        Some(Sub::Run { commands }) => {
            // Asking for help runs nothing, so that a command line copied
            // with -h on the end cannot change a file.
            let asked: Vec<_> = commands
                .iter()
                .filter_map(|line| help_request(line))
                .collect();
            if !asked.is_empty() {
                let mut pages = Vec::new();
                for page in asked {
                    let page = page.map_err(anyhow::Error::msg)?;
                    pages.push(format!("{}\n{}", page.title, page.lines.join("\n")));
                }
                println!("{}", pages.join("\n\n"));
                return Ok(());
            }
            // Read every command before running any, so that a mistake in
            // a later one does not leave the earlier ones half done.
            let commands = commands
                .iter()
                .map(|line| Command::parse_line(line))
                .collect::<Result<Vec<_>, _>>()?;
            let mut session = Session::new(cli.log.map(SessionLog::new));
            for command in commands {
                let outcome = session.execute(command)?;
                println!("{}", outcome.message);
            }
        }
        None => {
            // A file that is missing or of no known kind is reported here,
            // before the screen is taken over and before any log exists.
            for path in &cli.files {
                check_input(path, cli.kind)?;
            }
            // Checked before anything runs, so a failed start changes nothing.
            if !std::io::stdout().is_terminal() {
                bail!(
                    "the interface needs a terminal; use `seqpeeq run` or `seqpeeq replay` in scripts"
                );
            }
            let log = if cli.no_log {
                None
            } else {
                let path = match cli.log {
                    Some(path) => path,
                    None => SessionLog::default_path(&std::env::current_dir()?, &Zoned::now()),
                };
                Some(SessionLog::new(path))
            };
            // The files are read once the interface is up, in the
            // background, so a large one does not delay it.
            let mut app = app::App::new(Session::new(log));
            for command in open_commands(cli.files, cli.kind, cli.reference)? {
                app.enqueue(command);
            }
            term::run(&mut app).context("the interface stopped")?;
        }
    }
    Ok(())
}

/// The `open` command for each file named on the command line, each BAM
/// followed by `bam ref` when a reference was given.
fn open_commands(
    files: Vec<PathBuf>,
    kind: Option<Kind>,
    reference: Option<PathBuf>,
) -> anyhow::Result<Vec<Command>> {
    let mut commands = Vec::new();
    let mut bams = 0;
    for path in files {
        let is_bam = kind.or_else(|| Kind::from_path(&path)) == Some(Kind::Bam);
        commands.push(Command::Open { path, kind });
        if let (true, Some(file)) = (is_bam, &reference) {
            commands.push(Command::Bam(BamCmd::Ref { file: file.clone() }));
            bams += 1;
        }
    }
    if reference.is_some() && bams == 0 {
        anyhow::bail!("--ref is for BAM files; none was given");
    }
    Ok(commands)
}

/// A test's wall-clock budget: `ms` on a developer's machine, ten times
/// that on a shared CI runner (GitHub Actions sets `CI`), where a debug
/// build shares its CPU. A budget is there to catch work that grows with
/// the input, which is far slower than either.
#[cfg(test)]
pub(crate) fn time_budget(ms: u64) -> std::time::Duration {
    budget_for(ms, std::env::var_os("CI").is_some())
}

#[cfg(test)]
fn budget_for(ms: u64, ci: bool) -> std::time::Duration {
    std::time::Duration::from_millis(if ci { ms * 10 } else { ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_budgets_stretch_on_a_shared_runner() {
        assert_eq!(budget_for(50, false), std::time::Duration::from_millis(50));
        assert_eq!(budget_for(50, true), std::time::Duration::from_millis(500));
    }

    #[test]
    fn kind_flag_applies_to_files_given_at_start() {
        let cli = Cli::try_parse_from(["seqpeeq", "x.txt", "--kind", "tree"]).unwrap();
        assert_eq!(
            open_commands(cli.files, cli.kind, cli.reference).unwrap(),
            [Command::Open {
                path: "x.txt".into(),
                kind: Some(Kind::Tree)
            }]
        );
    }

    #[test]
    fn files_without_kind_flag_are_detected_by_extension() {
        let cli = Cli::try_parse_from(["seqpeeq", "a.fa", "t.nwk"]).unwrap();
        assert_eq!(
            open_commands(cli.files, cli.kind, cli.reference).unwrap(),
            [
                Command::Open {
                    path: "a.fa".into(),
                    kind: None
                },
                Command::Open {
                    path: "t.nwk".into(),
                    kind: None
                },
            ]
        );
    }

    #[test]
    fn ref_follows_each_bam_opened_at_start() {
        let cli =
            Cli::try_parse_from(["seqpeeq", "a.bam", "t.nwk", "b.bam", "--ref", "r.fa"]).unwrap();
        let open = |p: &str| Command::Open {
            path: p.into(),
            kind: None,
        };
        let reference = || {
            Command::Bam(BamCmd::Ref {
                file: "r.fa".into(),
            })
        };
        assert_eq!(
            open_commands(cli.files, cli.kind, cli.reference).unwrap(),
            [
                open("a.bam"),
                reference(),
                open("t.nwk"),
                open("b.bam"),
                reference()
            ]
        );
        let cli = Cli::try_parse_from(["seqpeeq", "t.nwk", "--ref", "r.fa"]).unwrap();
        assert_eq!(
            open_commands(cli.files, cli.kind, cli.reference)
                .unwrap_err()
                .to_string(),
            "--ref is for BAM files; none was given"
        );
    }
}
