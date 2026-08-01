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

    /// Database connection lock was poisoned.
    #[error("Database lock error: {0}")]
    LockPoison(String),

    /// File watcher failure.
    #[error("Watcher error: {0}")]
    Watcher(#[from] notify::Error),

    /// System tray creation or event loop failure.
    #[error("Tray error: {0}")]
    Tray(String),
}

impl SyncError {
    /// Returns `true` if the error represents an SMB/network connectivity loss.
    ///
    /// Inspects the underlying Win32 error code from `std::io::Error::raw_os_error()`
    /// for known Windows network error codes.
    pub fn is_network_offline(&self) -> bool {
        match self {
            SyncError::Io(io_err) => matches!(
                io_err.raw_os_error(),
                Some(53)   // ERROR_BAD_NETPATH
                | Some(59) // ERROR_UNEXP_NET_ERR
                | Some(64) // ERROR_NETNAME_DELETED
                | Some(67) // ERROR_BAD_NET_NAME
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_network_offline_true() {
        let err = SyncError::Io(std::io::Error::from_raw_os_error(67));
        assert!(err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_false_for_other_io() {
        let err = SyncError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert!(!err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_false_for_non_io() {
        let err = SyncError::Validation("some error".to_string());
        assert!(!err.is_network_offline());
    }
}
