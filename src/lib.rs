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
//!   integration via [`run_tray`].
//!
//! ## Core Modules
//!
//! - [`config`]: Configuration parsing, validation, and [`config::ConfigBuilder`].
//! - [`daemon`]: Background worker orchestration, lifecycle control, and tray action dispatch.
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
//! ```no_run
//! use std::path::Path;
//! use syncdir::config::Config;
//! use syncdir::daemon::SyncDaemon;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = Config::builder("C:\\Source")
//!         .dest_dir("\\\\server\\share\\Dest")
//!         .build();
//!
//!     let app_dir = Path::new("C:\\ProgramData\\syncdir");
//!     let daemon = SyncDaemon::start(config, app_dir, None)?;
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
pub mod tray;

pub use daemon::{DaemonTrayHandler, SyncDaemon};

/// Copyright notice for syncdir.
pub const COPYRIGHT: &str = "(c) 2026 Wendell Saligan";
