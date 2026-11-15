//! Core of seqpeeq: the command language, session log and replay.
//!
//! This crate has no terminal dependencies. Interfaces build [`Command`]s and
//! hand them to a session; they never change data directly.

pub mod command;
pub mod error;
pub mod log;
pub mod session;

pub use command::{Command, Kind};
pub use error::{Error, Result};
pub use log::SessionLog;
pub use session::{Dataset, Outcome, Session};
