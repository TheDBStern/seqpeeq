//! The `seqpeeq` command: interactive viewer, `replay` and `run`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use jiff::Zoned;
use seqpeeq_core::{Command, Session, SessionLog, replay};

/// View and edit alignments, trees and mapped reads in the terminal.
#[derive(Parser)]
#[command(name = "seqpeeq", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    sub: Option<Sub>,

    /// Files to open (FASTA alignment, Newick tree, BAM)
    files: Vec<PathBuf>,

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
            let noun = if report.commands == 1 { "command" } else { "commands" };
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
            for path in cli.files {
                session.execute(Command::Open { path, kind: None })?;
            }
            println!("interface not built yet");
        }
    }
    Ok(())
}
