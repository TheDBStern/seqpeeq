//! Core of seqpeeq: the command language, session log and replay.
//!
//! This crate has no terminal dependencies. Interfaces build [`Command`]s and
//! hand them to a session; they never change data directly.

pub mod command;
pub mod error;
pub mod log;
pub mod msa;
pub mod ranges;
pub mod replay;
pub mod session;

pub use command::msa::{MsaCmd, RowSel};
pub use command::{Command, Kind};
pub use error::{Error, Result};
pub use log::SessionLog;
pub use msa::{Alignment, Alphabet};
pub use ranges::Ranges;
pub use replay::{ReplayReport, replay};
pub use session::{Content, Dataset, Outcome, Session};
