//! # syncdir
//!
//! A lightweight Windows user-session background sync utility with block-level delta synchronization.
//!
//! Mirrors a local source folder to one or more target destination directories (such as local folders
//! or mapped SMB network shares) in real-time. It operates within the user login session to inherit
//! appropriate user permissions for accessing network drives.
//!
//! ## Overview
//!
//! - **Delta Synchronization**: Files $\ge$ 10MB are segmented into 1MB blocks and synchronized in-place
//!   using Blake3 cryptographic hashes stored in a local SQLite cache.
//! - **Small File Optimization**: Files < 10MB bypass block hashing and perform fast-path whole-file copy.
//! - **Real-Time Directory Monitoring**: Leverages Windows `ReadDirectoryChangesW` notifications via `notify`.
//! - **Resilient Windows Networking**: Bidirectional UNC $\longleftrightarrow$ mapped drive translation and
//!   session authentication via Win32 `WNetAddConnection2W` and `WNetGetConnectionW`.
//! - **Daemon Orchestration**: Clean lifecycle management via [`SyncDaemon`] and windowless system tray
//!   integration via [`tray::TrayEventLoop::run`].
//!
//! ## Core Modules
//!
//! - [`config`]: Configuration parsing, validation, and [`config::ConfigBuilder`].
//! - [`daemon`]: Background worker orchestration and lifecycle control.
//! - [`db`]: SQLite-backed signature cache and [`db::HashStore`] trait.
//! - [`error`]: Typed error hierarchy and causal error chaining with [`error::SyncError`].
//! - [`monitor`]: Real-time filesystem watcher and debounced event dispatch.
//! - [`net`]: Win32 UNC and SMB connection resolution.
//! - [`path_util`]: Path canonicalization, slash normalization, and UNC repair.
//! - [`startup`]: Windows registry auto-start integration.
//! - [`sync`]: Streaming delta sync engine, [`sync::DirtyBlockRange`] coalescing, and worker routines.
//! - [`tray`]: System tray notification area icon, tooltip state machine, and context menus.
//!
//! ## Examples
//!
//! ### Constructing and Validating Configuration
//!
//! ```
//! use syncdir::config::Config;
//! use std::path::Path;
//!
//! let temp = tempfile::tempdir().unwrap();
//! let src = temp.path().join("source");
//! let dst = temp.path().join("dest");
//! std::fs::create_dir_all(&src).unwrap();
//! std::fs::create_dir_all(&dst).unwrap();
//!
//! let config = Config::builder(&src)
//!     .dest_dir(&dst)
//!     .propagate_deletions(true)
//!     .build()
//!     .unwrap();
//!
//! assert_eq!(config.dest_dirs().unwrap().len(), 1);
//! assert!(config.propagate_deletions());
//! ```
//!
//! ### Type-Safe Relative Path Validation
//!
//! ```
//! use syncdir::path_util::RelativePath;
//! use std::path::Path;
//!
//! let rel = RelativePath::try_new("reports/summary.docx").unwrap();
//! assert_eq!(rel.as_path(), Path::new("reports/summary.docx"));
//! ```
//!
//! ### Configuring and Running the Daemon
//!
//! ```no_run
//! use std::path::Path;
//! use syncdir::config::Config;
//! use syncdir::daemon::SyncDaemon;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = Config::builder("C:\\Source")
//!         .dest_dir("\\\\server\\share\\Dest")
//!         .build()?;
//!
//!     let app_dir = Path::new("C:\\ProgramData\\syncdir");
//!     let daemon = SyncDaemon::builder(config, app_dir).start()?;
//!
//!     // Keep running until shutdown signal
//!     daemon.shutdown();
//!     Ok(())
//! }
//! ```

pub mod config;
pub mod daemon;
pub mod db;
pub mod error;
pub mod monitor;
pub mod net;
pub mod path_util;
pub mod startup;
pub mod sync;
#[doc(hidden)]
pub mod test_support;
pub mod tray;

pub use daemon::{DaemonHandle, SyncDaemon, SyncDaemonBuilder};

/// Copyright notice for syncdir.
pub const COPYRIGHT: &str = "(c) 2026 Wendell Saligan";
