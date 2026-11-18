//! Core of seqpeeq: the command language, session log and replay.
//!
//! This crate has no terminal dependencies. Interfaces build [`Command`]s and
//! hand them to a session; they never change data directly.

pub mod bam;
pub mod command;
pub mod error;
pub mod log;
pub mod msa;
mod pair;
pub mod progress;
pub mod ranges;
pub mod replay;
pub mod session;
pub mod tree;
mod write;

pub use command::msa::{MsaCmd, RowSel};
pub use command::tree::{NodeSel, TreeCmd};
pub use command::{Command, Kind};
pub use error::{Error, Result};
pub use log::SessionLog;
pub use msa::{
    Alignment, Alphabet, Conservation, SUMMARY_BYTES, Shift, Summary, Tally, View, summary_block,
};
pub use progress::Progress;
pub use ranges::Ranges;
pub use replay::{ReplayReport, replay};
pub use session::{
    Content, Dataset, Finished, Job, LinkSummary, Outcome, Session, Started, check_input,
};
pub use tree::{NodeId, Parsed, Tree, TreeDoc, TreeFacts};
