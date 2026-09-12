//! Unified error types for the syncdir crate.

use thiserror::Error;

fn format_block_detail(block_index: &Option<u64>) -> String {
    match block_index {
        Some(idx) => format!(" (block {})", idx),
        None => String::new(),
    }
}

/// Categorization of validation errors for recovery and failure classification.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValidationKind {
    /// Security violation (e.g. unsafe traversal, directory traversal).
    Security,
    /// Path traversal attempt.
    PathTraversal,
    /// Reparse point or directory junction violation.
    ReparsePoint,
    /// Filename contains reserved Windows DOS device name.
    ReservedDeviceName,
    /// Recursive sync loop (destination inside source or source inside destination).
    RecursiveLoop,
    /// Domain invariant violation (e.g. invalid parameter, zero debounce).
    Invariant,
    /// Transient validation error (e.g. temporary lock conflict).
    Transient,
}

impl ValidationKind {
    /// Returns `true` if the validation error is permanent and should not be retried.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::Security
                | Self::PathTraversal
                | Self::ReparsePoint
                | Self::ReservedDeviceName
                | Self::RecursiveLoop
        )
    }
}

/// Subsystem error type for directory watching and filesystem notification failures.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// Failure originated from the underlying `notify` watcher.
    #[error(transparent)]
    Notify(#[from] notify::Error),

    /// The watched path does not exist.
    #[error("Watched path not found: {0}")]
    PathNotFound(std::path::PathBuf),

    /// Watcher event notification channel disconnected.
    #[error("Watcher channel disconnected: {0}")]
    ChannelDisconnected(String),

    /// Generic watcher error with optional underlying cause.
    #[error("Watcher error: {0}")]
    Other(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),
}

/// All fallible operations in syncdir return this error type.
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum SyncError {
    /// Filesystem I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// SQLite database operation failure.
    #[error("Database error: {0}")]
    Db(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    /// Configuration file parsing failure.
    #[error("Config error: {0}")]
    Config(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    /// Runtime validation failure (e.g. missing directories, invariant violations).
    #[error("Validation error: {message}")]
    Validation {
        /// Classification of validation failure.
        kind: ValidationKind,
        /// Explanatory failure message.
        message: String,
    },

    /// Write verification failed for a path (data mismatch).
    #[error("Write verification failed for: {path}{}", format_block_detail(.block_index))]
    WriteVerificationFailed {
        /// The path where verification failed.
        path: std::path::PathBuf,
        /// The specific block index that failed verification, if applicable.
        block_index: Option<u64>,
        /// The expected Blake3 hash of the block.
        expected_hash: Option<[u8; 32]>,
        /// The actual Blake3 hash read back from the destination.
        actual_hash: Option<[u8; 32]>,
    },

    /// Lock was poisoned.
    #[error("Lock poisoned: {0}")]
    LockPoison(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    /// File watcher failure.
    #[error(transparent)]
    Watcher(#[from] WatcherError),

    /// System tray creation or event loop failure.
    #[error("Tray error: {0}")]
    Tray(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    /// Windows startup registry operation failure.
    #[error("Registry error: {0}")]
    Registry(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    /// Operation was cancelled cooperatively.
    #[error("Operation cancelled")]
    Cancelled,
}

impl From<rusqlite::Error> for SyncError {
    fn from(err: rusqlite::Error) -> Self {
        SyncError::Db(err.to_string(), Some(Box::new(err)))
    }
}

impl From<notify::Error> for SyncError {
    fn from(err: notify::Error) -> Self {
        SyncError::Watcher(WatcherError::Notify(err))
    }
}

impl From<toml::de::Error> for SyncError {
    fn from(err: toml::de::Error) -> Self {
        SyncError::Config(err.to_string(), Some(Box::new(err)))
    }
}

/// Returns `true` if an `io::Error` represents an SMB/network connectivity loss.
///
/// Inspects both standard [`std::io::ErrorKind`] variants (such as `TimedOut`, `ConnectionReset`,
/// `BrokenPipe`, `NetworkUnreachable`) and Win32 raw OS error codes (such as `ERROR_BAD_NETPATH`,
/// `ERROR_NETNAME_DELETED`, `ERROR_NETWORK_UNREACHABLE`).
///
/// # Arguments
///
/// * `io_err` - Reference to the [`std::io::Error`] to inspect.
///
/// # Returns
///
/// `true` if the error indicates a transient or persistent network disconnect, `false` otherwise.
#[must_use]
pub fn is_network_offline_io(io_err: &std::io::Error) -> bool {
    matches!(
        io_err.kind(),
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkDown
            | std::io::ErrorKind::ConnectionRefused
    ) || matches!(
        io_err.raw_os_error(),
        Some(15) // ERROR_INVALID_DRIVE
        | Some(53) // ERROR_BAD_NETPATH
        | Some(59) // ERROR_UNEXP_NET_ERR
        | Some(64) // ERROR_NETNAME_DELETED
        | Some(65) // ERROR_NETWORK_ACCESS_DENIED (network busy)
        | Some(67) // ERROR_BAD_NET_NAME
        | Some(121) // ERROR_SEM_TIMEOUT
        | Some(1222) // ERROR_NO_NETWORK
        | Some(1231) // ERROR_NETWORK_UNREACHABLE
        | Some(1232) // ERROR_HOST_UNREACHABLE
        | Some(1326) // ERROR_LOGON_FAILURE
    )
}

impl SyncError {
    /// Create a `SyncError::Config` without a source cause.
    #[must_use]
    pub fn config(msg: impl Into<String>) -> Self {
        SyncError::Config(msg.into(), None)
    }

    /// Create a `SyncError::Config` with an underlying source error cause.
    #[must_use]
    pub fn config_with_source<E: std::error::Error + Send + Sync + 'static>(
        msg: impl Into<String>,
        source: E,
    ) -> Self {
        SyncError::Config(msg.into(), Some(Box::new(source)))
    }

    /// Create a `SyncError::Validation` error with default Invariant classification.
    #[must_use]
    pub fn validation(msg: impl Into<String>) -> Self {
        SyncError::Validation {
            kind: ValidationKind::Invariant,
            message: msg.into(),
        }
    }

    /// Create a `SyncError::Validation` error with explicit classification kind.
    #[must_use]
    pub fn validation_kind(kind: ValidationKind, msg: impl Into<String>) -> Self {
        SyncError::Validation {
            kind,
            message: msg.into(),
        }
    }

    /// Create a `SyncError::Validation` error for security violations.
    ///
    /// Used when path safety, reparse points, junctions, or directory traversal invariants
    /// are violated, signaling a potential security risk.
    ///
    /// # Arguments
    ///
    /// * `msg` - Explanatory message detailing the security violation.
    ///
    /// # Returns
    ///
    /// A [`SyncError::Validation`] instance wrapping the message string.
    ///
    /// # Examples
    ///
    /// ```
    /// use syncdir::error::SyncError;
    ///
    /// let err = SyncError::validation_security("Unsafe path traversal detected: ../secret");
    /// assert!(err.is_permanent_validation_failure());
    /// ```
    #[must_use]
    pub fn validation_security(msg: impl Into<String>) -> Self {
        SyncError::Validation {
            kind: ValidationKind::Security,
            message: msg.into(),
        }
    }

    /// Create a `SyncError::Validation` error for reparse point or junction violations.
    #[must_use]
    pub fn validation_reparse(msg: impl Into<String>) -> Self {
        SyncError::Validation {
            kind: ValidationKind::ReparsePoint,
            message: msg.into(),
        }
    }

    /// Create a `SyncError::Validation` error for domain invariant violations.
    #[must_use]
    pub fn validation_invariant(msg: impl Into<String>) -> Self {
        SyncError::Validation {
            kind: ValidationKind::Invariant,
            message: msg.into(),
        }
    }

    /// Create a `SyncError::Validation` error for recursive sync loop violations.
    #[must_use]
    pub fn validation_loop(msg: impl Into<String>) -> Self {
        SyncError::Validation {
            kind: ValidationKind::RecursiveLoop,
            message: msg.into(),
        }
    }

    /// Check if this error is a permanent validation failure that should not be retried.
    #[must_use]
    pub fn is_permanent_validation_failure(&self) -> bool {
        match self {
            Self::Validation { kind, .. } => kind.is_permanent(),
            _ => false,
        }
    }

    /// Create a `SyncError::WriteVerificationFailed` error.
    #[must_use]
    pub fn write_verification_failed(path: impl Into<std::path::PathBuf>) -> Self {
        SyncError::WriteVerificationFailed {
            path: path.into(),
            block_index: None,
            expected_hash: None,
            actual_hash: None,
        }
    }

    /// Create an enriched `SyncError::WriteVerificationFailed` error with block diagnostics.
    #[must_use]
    pub fn write_verification_failed_block(
        path: impl Into<std::path::PathBuf>,
        block_index: u64,
        expected: [u8; 32],
        actual: [u8; 32],
    ) -> Self {
        SyncError::WriteVerificationFailed {
            path: path.into(),
            block_index: Some(block_index),
            expected_hash: Some(expected),
            actual_hash: Some(actual),
        }
    }

    /// Create a `SyncError::Db` without a source cause.
    #[must_use]
    pub fn db(msg: impl Into<String>) -> Self {
        SyncError::Db(msg.into(), None)
    }

    /// Create a `SyncError::Db` with an underlying source error cause.
    #[must_use]
    pub fn db_with_source<E: std::error::Error + Send + Sync + 'static>(
        msg: impl Into<String>,
        source: E,
    ) -> Self {
        SyncError::Db(msg.into(), Some(Box::new(source)))
    }

    /// Create a `SyncError::LockPoison` without a source cause.
    #[must_use]
    pub fn lock_poison(msg: impl Into<String>) -> Self {
        SyncError::LockPoison(msg.into(), None)
    }

    /// Create a `SyncError::LockPoison` with an underlying source error cause.
    #[must_use]
    pub fn lock_poison_with_source<E: std::error::Error + Send + Sync + 'static>(
        msg: impl Into<String>,
        source: E,
    ) -> Self {
        SyncError::LockPoison(msg.into(), Some(Box::new(source)))
    }

    /// Create a `SyncError::Watcher` without a source cause.
    #[must_use]
    pub fn watcher(msg: impl Into<String>) -> Self {
        SyncError::Watcher(WatcherError::Other(msg.into(), None))
    }

    /// Create a `SyncError::Watcher` with an underlying source error cause.
    #[must_use]
    pub fn watcher_with_source<E: std::error::Error + Send + Sync + 'static>(
        msg: impl Into<String>,
        source: E,
    ) -> Self {
        SyncError::Watcher(WatcherError::Other(msg.into(), Some(Box::new(source))))
    }

    /// Create a `SyncError::Tray` without a source cause.
    #[must_use]
    pub fn tray(msg: impl Into<String>) -> Self {
        SyncError::Tray(msg.into(), None)
    }

    /// Create a `SyncError::Tray` with an underlying source error cause.
    #[must_use]
    pub fn tray_with_source<E: std::error::Error + Send + Sync + 'static>(
        msg: impl Into<String>,
        source: E,
    ) -> Self {
        SyncError::Tray(msg.into(), Some(Box::new(source)))
    }

    /// Create a `SyncError::Registry` without a source cause.
    #[must_use]
    pub fn registry(msg: impl Into<String>) -> Self {
        SyncError::Registry(msg.into(), None)
    }

    /// Create a `SyncError::Registry` with an underlying source error cause.
    #[must_use]
    pub fn registry_with_source<E: std::error::Error + Send + Sync + 'static>(
        msg: impl Into<String>,
        source: E,
    ) -> Self {
        SyncError::Registry(msg.into(), Some(Box::new(source)))
    }

    /// Create a `SyncError::Cancelled` error.
    #[must_use]
    pub fn cancelled() -> Self {
        SyncError::Cancelled
    }

    /// Create a `SyncError::Io` error.
    #[must_use]
    pub fn io(err: std::io::Error) -> Self {
        SyncError::Io(err)
    }

    /// Create a generic `SyncError::Config` error message.
    #[must_use]
    pub fn generic(msg: impl Into<String>) -> Self {
        SyncError::Config(msg.into(), None)
    }

    /// Returns `true` if the error represents a cooperative cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, SyncError::Cancelled)
    }

    /// Returns `true` if the error represents an SMB/network connectivity loss.
    #[must_use]
    pub fn is_network_offline(&self) -> bool {
        match self {
            SyncError::Io(io_err) => is_network_offline_io(io_err),
            _ => false,
        }
    }

    /// Returns `true` if the error represents an I/O or watched path `NotFound` condition.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        match self {
            SyncError::Io(io_err) => io_err.kind() == std::io::ErrorKind::NotFound,
            SyncError::Watcher(WatcherError::PathNotFound(_)) => true,
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
    fn test_is_network_offline_network_busy() {
        let err = SyncError::Io(std::io::Error::from_raw_os_error(65));
        assert!(err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_sem_timeout() {
        let err = SyncError::Io(std::io::Error::from_raw_os_error(121));
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
        let err = SyncError::validation("some error");
        assert!(!err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_logon_failure() {
        let err = SyncError::Io(std::io::Error::from_raw_os_error(1326));
        assert!(err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_path_not_found() {
        let err = SyncError::Io(std::io::Error::from_raw_os_error(3));
        // Error code 3 (ERROR_PATH_NOT_FOUND) is local missing path, not network offline
        assert!(!err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_invalid_drive() {
        let err = SyncError::Io(std::io::Error::from_raw_os_error(15));
        assert!(err.is_network_offline());
    }

    #[test]
    fn test_is_network_offline_io_function() {
        let err = std::io::Error::from_raw_os_error(53);
        assert!(is_network_offline_io(&err));
        let err_other = std::io::Error::other("other");
        assert!(!is_network_offline_io(&err_other));
    }

    #[test]
    fn test_is_network_offline_io_win32_network_unreachable_codes() {
        assert!(is_network_offline_io(&std::io::Error::from_raw_os_error(
            1222
        )));
        assert!(is_network_offline_io(&std::io::Error::from_raw_os_error(
            1231
        )));
        assert!(is_network_offline_io(&std::io::Error::from_raw_os_error(
            1232
        )));
        assert!(!is_network_offline_io(&std::io::Error::from_raw_os_error(
            2
        )));
    }

    #[test]
    fn test_from_rusqlite_error() {
        let sqlite_err = rusqlite::Error::QueryReturnedNoRows;
        let sync_err: SyncError = sqlite_err.into();
        assert!(matches!(sync_err, SyncError::Db(_, Some(_))));
    }

    #[test]
    fn test_from_notify_error() {
        let notify_err = notify::Error::generic("watch error");
        let sync_err: SyncError = notify_err.into();
        assert!(matches!(
            sync_err,
            SyncError::Watcher(WatcherError::Notify(_))
        ));
    }

    #[test]
    fn test_from_watcher_error_for_sync_error_causal_chain() {
        use std::error::Error;

        let path = std::path::PathBuf::from("C:\\missing\\path");
        let w_err = WatcherError::PathNotFound(path.clone());
        let sync_err: SyncError = w_err.into();

        assert!(matches!(
            sync_err,
            SyncError::Watcher(WatcherError::PathNotFound(_))
        ));
        assert!(sync_err.to_string().contains("Watched path not found"));

        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let sync_err2 = SyncError::watcher_with_source("IO failure during watch", io_err);
        assert!(sync_err2.source().is_some());
        assert!(sync_err2.to_string().contains("IO failure during watch"));
    }

    #[test]
    fn test_from_toml_error() {
        let toml_err: Result<toml::Value, _> = toml::from_str("invalid = [");
        let toml_err = toml_err.unwrap_err();
        let sync_err: SyncError = toml_err.into();
        assert!(matches!(sync_err, SyncError::Config(_, Some(_))));
    }

    #[test]
    fn test_sync_error_constructors() {
        let err = SyncError::config("cfg error");
        assert_eq!(err.to_string(), "Config error: cfg error");

        let err = SyncError::validation("val error");
        assert_eq!(err.to_string(), "Validation error: val error");

        let err = SyncError::db("db error");
        assert_eq!(err.to_string(), "Database error: db error");

        let err = SyncError::lock_poison("lock poisoned");
        assert_eq!(err.to_string(), "Lock poisoned: lock poisoned");

        let err = SyncError::watcher("watcher error");
        assert_eq!(err.to_string(), "Watcher error: watcher error");

        let err = SyncError::tray("tray error");
        assert_eq!(err.to_string(), "Tray error: tray error");

        let err = SyncError::registry("reg error");
        assert_eq!(err.to_string(), "Registry error: reg error");

        let err = SyncError::write_verification_failed(std::path::PathBuf::from("test/file.txt"));
        assert_eq!(
            err.to_string(),
            "Write verification failed for: test/file.txt"
        );
        assert!(matches!(err, SyncError::WriteVerificationFailed { .. }));
    }

    #[test]
    fn test_tray_with_source_preserves_chain() {
        use std::error::Error;
        let io_err = std::io::Error::other("underlying");
        let err = SyncError::tray_with_source("tray failed", io_err);
        assert!(err.source().is_some());
        assert_eq!(err.to_string(), "Tray error: tray failed");
    }

    #[test]
    fn test_registry_with_source_preserves_chain() {
        use std::error::Error;
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = SyncError::registry_with_source("registry failed", io_err);
        assert!(err.source().is_some());
        assert_eq!(err.to_string(), "Registry error: registry failed");
    }

    #[test]
    fn test_lock_poison_with_source_preserves_chain() {
        use std::error::Error;
        let poison = std::sync::PoisonError::new(());
        let err = SyncError::lock_poison_with_source("lock failed", poison);
        assert!(err.source().is_some());
        assert_eq!(err.to_string(), "Lock poisoned: lock failed");
    }

    #[test]
    fn test_lock_poison_without_source() {
        use std::error::Error;
        let err = SyncError::lock_poison("lock failed");
        assert!(err.source().is_none());
    }

    #[test]
    fn test_write_verification_diagnostics() {
        let hash_a: [u8; 32] = [0xAA; 32];
        let hash_b: [u8; 32] = [0xBB; 32];
        let err = SyncError::WriteVerificationFailed {
            path: std::path::PathBuf::from("test/file.bin"),
            block_index: Some(42),
            expected_hash: Some(hash_a),
            actual_hash: Some(hash_b),
        };
        let msg = err.to_string();
        assert!(msg.contains("test/file.bin"));
        assert!(msg.contains("block 42"));
        assert!(matches!(
            err,
            SyncError::WriteVerificationFailed {
                block_index: Some(42),
                ..
            }
        ));
    }

    #[test]
    fn test_is_network_offline_io_kinds() {
        use std::io::{Error, ErrorKind};

        let network_kinds = [
            ErrorKind::TimedOut,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::NotConnected,
            ErrorKind::BrokenPipe,
            ErrorKind::NetworkUnreachable,
            ErrorKind::HostUnreachable,
            ErrorKind::NetworkDown,
            ErrorKind::ConnectionRefused,
        ];

        for kind in network_kinds {
            let io_err = Error::new(kind, "network dropped");
            assert!(
                is_network_offline_io(&io_err),
                "Expected is_network_offline_io to return true for {:?}",
                kind
            );
        }

        let non_network_kinds = [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::AlreadyExists,
            ErrorKind::InvalidData,
        ];

        for kind in non_network_kinds {
            let io_err = Error::new(kind, "filesystem error");
            assert!(
                !is_network_offline_io(&io_err),
                "Expected is_network_offline_io to return false for {:?}",
                kind
            );
        }
    }

    #[test]
    fn test_sync_error_permanent_validation_failure_classification() {
        let perm1 = SyncError::validation_security("Unsafe path traversal detected: ../secret");
        assert!(perm1.is_permanent_validation_failure());

        let perm2 = SyncError::validation_security(
            "Destination component 'C:\\dest\\junction' is a symlink or reparse point; refusing to write",
        );
        assert!(perm2.is_permanent_validation_failure());

        let perm3 =
            SyncError::validation_security("Filename contains reserved DOS device name: AUX");
        assert!(perm3.is_permanent_validation_failure());

        let transient = SyncError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "network timeout",
        ));
        assert!(!transient.is_permanent_validation_failure());

        let transient_val = SyncError::validation_kind(
            ValidationKind::Transient,
            "Temporary lock acquisition delay",
        );
        assert!(!transient_val.is_permanent_validation_failure());
    }

    #[test]
    fn test_validation_kind_permanent_classification() {
        use super::ValidationKind;

        assert!(ValidationKind::Security.is_permanent());
        assert!(ValidationKind::PathTraversal.is_permanent());
        assert!(ValidationKind::ReparsePoint.is_permanent());
        assert!(ValidationKind::ReservedDeviceName.is_permanent());
        assert!(ValidationKind::RecursiveLoop.is_permanent());
        assert!(!ValidationKind::Invariant.is_permanent());
        assert!(!ValidationKind::Transient.is_permanent());
    }

    #[test]
    fn test_validation_kind_sync_error_integration() {
        use super::ValidationKind;

        let err_sec = SyncError::validation_kind(ValidationKind::Security, "auth violation");
        assert!(err_sec.is_permanent_validation_failure());
        assert_eq!(err_sec.to_string(), "Validation error: auth violation");

        let err_inv = SyncError::validation_invariant("invariant violation");
        assert!(!err_inv.is_permanent_validation_failure());
        assert_eq!(err_inv.to_string(), "Validation error: invariant violation");
    }

    #[test]
    fn test_sync_error_must_use_and_is_not_found() {
        let io_nf = SyncError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(io_nf.is_not_found());

        let io_other = SyncError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(!io_other.is_not_found());

        let config_err = SyncError::config("bad config");
        assert!(!config_err.is_not_found());
    }

    #[test]
    fn test_watcher_error_non_exhaustive_and_construction() {
        let err_path =
            WatcherError::PathNotFound(std::path::PathBuf::from(r"C:\Missing\WatchedDir"));
        let err_chan = WatcherError::ChannelDisconnected("Worker channel closed".to_string());
        let err_other = WatcherError::Other("General watch failure".to_string(), None);

        #[allow(unreachable_patterns)]
        let desc = match &err_path {
            WatcherError::Notify(_) => "notify",
            WatcherError::PathNotFound(p) => {
                assert_eq!(p, &std::path::PathBuf::from(r"C:\Missing\WatchedDir"));
                "not_found"
            }
            WatcherError::ChannelDisconnected(_) => "disconnected",
            WatcherError::Other(msg, _) => {
                assert!(!msg.is_empty());
                "other"
            }
            _ => "future_variant",
        };
        assert_eq!(desc, "not_found");

        assert!(format!("{}", err_chan).contains("Watcher channel disconnected"));
        assert!(format!("{}", err_other).contains("Watcher error"));

        let sync_err: SyncError = err_path.into();
        assert!(sync_err.is_not_found());
        assert!(format!("{}", sync_err).contains("Watched path not found"));
    }
}
