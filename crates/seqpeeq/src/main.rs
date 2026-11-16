//! The `seqpeeq` command: interactive viewer, `replay` and `run`.

mod app;
mod line;
mod term;
mod ui;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use jiff::Zoned;
use seqpeeq_core::{Command, Kind, Session, SessionLog, replay};

/// View and edit alignments, trees and mapped reads in the terminal.
#[derive(Parser)]
#[command(name = "seqpeeq", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    sub: Option<Sub>,

    /// Files to open (FASTA alignment, Newick tree, BAM)
    files: Vec<PathBuf>,

    /// Treat the files as this kind instead of going by their extensions
    #[arg(long, value_enum)]
    kind: Option<Kind>,

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
    match cli.sub {
        Some(Sub::Replay { log_file }) => {
            let text = std::fs::read_to_string(&log_file)
                .with_context(|| format!("cannot read {}", log_file.display()))?;
            let mut session = Session::new(cli.log.map(SessionLog::new));
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
            let mut session = Session::new(cli.log.map(SessionLog::new));
            for line in &commands {
                let outcome = session.execute(Command::parse_line(line)?)?;
                println!("{}", outcome.message);
            }
        }
        None => {
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
            let mut session = Session::new(log);
            for command in open_commands(cli.files, cli.kind) {
                session.execute(command)?;
            }
            term::run(&mut app::App::new(session)).context("the interface stopped")?;
        }
    }
    Ok(())
}

/// The `open` command for each file named on the command line.
fn open_commands(files: Vec<PathBuf>, kind: Option<Kind>) -> Vec<Command> {
    files
        .into_iter()
        .map(|path| Command::Open { path, kind })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_flag_applies_to_files_given_at_start() {
        let cli = Cli::try_parse_from(["seqpeeq", "x.txt", "--kind", "tree"]).unwrap();
        assert_eq!(
            open_commands(cli.files, cli.kind),
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
            open_commands(cli.files, cli.kind),
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
}
