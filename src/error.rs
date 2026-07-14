//! Unified error types for the syncdir crate.

use thiserror::Error;

/// All fallible operations in syncdir return this error type.
#[derive(Error, Debug)]
pub enum SyncError {
    /// Filesystem I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// SQLite database operation failure.
    #[error("Database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// Configuration file parsing failure.
    #[error("Config error: {0}")]
    Config(String),

    /// Runtime validation failure (e.g. missing directories).
    #[error("Validation error: {0}")]
    Validation(String),

    /// File watcher failure.
    #[error("Watcher error: {0}")]
    Watcher(#[from] notify::Error),
}
