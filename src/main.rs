//! syncdir — Windows background folder synchronization daemon.
//!
//! Mirrors a source folder to a destination folder using block-level
//! delta synchronization over the local network.

use std::fs;
use std::sync::mpsc::channel;
use syncdir::config::Config;
use syncdir::db::SqliteHashStore;
use syncdir::error::SyncError;
use syncdir::monitor::DirectoryWatcher;
use syncdir::sync::{start_sync_worker, SyncCommand};
use syncdir::tray::run_tray;
use tracing_subscriber::fmt::writer::MakeWriterExt;

fn try_main() -> Result<(), SyncError> {
    // 1. Ensure application directory exists
    let app_dir = Config::default_app_dir()?;
    if !app_dir.exists() {
        fs::create_dir_all(&app_dir).map_err(SyncError::Io)?;
    }

    // 2. Initialize tracing to stdout and a daily rolling log file
    let log_dir = app_dir.join("logs");
    if !log_dir.exists() {
        fs::create_dir_all(&log_dir).map_err(SyncError::Io)?;
    }
    let file_appender = tracing_appender::rolling::daily(&log_dir, "syncdir.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Combine stdout and rolling file writers
    let dual_writer = std::io::stdout.and(non_blocking);

    tracing_subscriber::fmt()
        .with_writer(dual_writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Initializing syncdir daemon...");

    // 3. Load or create configuration
    let config_path = Config::default_config_path()?;
    if !config_path.exists() {
        let default_toml = r#"# syncdir Configuration File

# The directory to monitor for changes.
source_dir = "C:\\path\\to\\source"

# The destination directory to synchronize changes to.
dest_dir = "C:\\path\\to\\destination"

# Debounce delay in seconds before performing a sync.
debounce_seconds = 2

# Whether to propagate file and directory deletions.
propagate_deletions = true

# Files larger than this threshold (in bytes) will use block-level delta sync.
# Smaller files are copied whole. (e.g. 10485760 = 10MB)
block_sync_threshold_bytes = 10485760

# The block size in bytes used for calculating delta signatures. (e.g. 65536 = 64KB)
block_size_bytes = 65536

# Verify file integrity after writes using rolling/blake3 checksums.
verify_writes = true
"#;
        fs::write(&config_path, default_toml).map_err(SyncError::Io)?;
        tracing::warn!(
            "Configuration file not found. Created default config at: {}",
            config_path.display()
        );
        tracing::warn!(
            "Please edit the configuration file with valid paths and restart the daemon."
        );
        return Ok(());
    }

    let config = Config::load(&config_path)?;
    tracing::info!("Loaded configuration from: {}", config_path.display());

    // Validate the config directories actually exist (or try to create/check them)
    if let Err(e) = config.validate() {
        tracing::error!("Configuration validation failed: {e}");
        return Err(e);
    }

    // 4. Initialize Database
    let db_path = app_dir.join("sigcache.db");
    tracing::info!("Opening signature cache database at: {}", db_path.display());
    let store = SqliteHashStore::new(&db_path, &config)?;

    // 5. Wire channel, watcher, and worker
    let (tx, rx) = channel();

    tracing::info!(
        "Starting background directory watcher on: {}",
        config.source_dir.display()
    );
    let _watcher = DirectoryWatcher::start(&config, tx.clone())?;

    tracing::info!("Starting sync worker thread...");
    let _worker_handle = start_sync_worker(config.clone(), store, rx);

    // Trigger initial sync scan
    let _ = tx.send(SyncCommand::TriggerFullScan);

    // 6. Run tray UI (blocks the main thread)
    tracing::info!("Starting system tray UI loop.");
    run_tray(config_path, log_dir, tx)?;

    tracing::info!("syncdir daemon shut down cleanly.");
    Ok(())
}

fn main() {
    if let Err(e) = try_main() {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}
