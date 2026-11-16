//! The error type shared by everything in the core.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The command line could not be understood. One line, no "error: " prefix.
    #[error("{0}")]
    Parse(String),

    /// A well-formed command that cannot be carried out. One line.
    #[error("{0}")]
    Invalid(String),

    #[error("cannot tell what kind of file {} is; add --kind msa|tree|bam", .0.display())]
    UnknownKind(PathBuf),

    // The causes below are part of the message, so they are deliberately not
    // exposed as `source()`: a caller printing the error chain would repeat them.
    #[error("{}: {cause}", .path.display())]
    Io {
        path: PathBuf,
        cause: std::io::Error,
    },

    /// Output files are only ever created, never replaced.
    #[error("{} already exists; choose another name or remove it first", .0.display())]
    Exists(PathBuf),

    /// The file is not in the format it was opened as.
    #[error("{}: {message}", .path.display())]
    Format { path: PathBuf, message: String },

    #[error("cannot write session log {}: {cause}; use --log PATH or --no-log", .path.display())]
    Log {
        path: PathBuf,
        cause: std::io::Error,
    },

    /// The log is one command per line, so such a command could not be replayed.
    #[error("command contains a line break, NUL or non-UTF-8 text and cannot be logged")]
    Unloggable,

    #[error("line {line}: {cause}")]
    Replay { line: usize, cause: Box<Error> },
}
