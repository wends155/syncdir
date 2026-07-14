//! syncdir — Windows background folder synchronization daemon.
//!
//! Mirrors a source folder to a destination folder using block-level
//! delta synchronization over the local network.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use syncdir::config::Config;
use syncdir::db::SqliteHashStore;
use syncdir::error::SyncError;
use syncdir::monitor::DirectoryWatcher;
use syncdir::sync::{SyncCommand, start_sync_worker};
use syncdir::tray::run_tray;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::fmt::writer::MakeWriterExt;

fn try_main(app_dir: PathBuf) -> Result<(), SyncError> {
    let log_dir = app_dir.join("logs");
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
debounce_seconds = 3

# Whether to propagate file and directory deletions.
propagate_deletions = true

# Files larger than this threshold (in bytes) will use block-level delta sync.
# Smaller files are copied whole. (e.g. 10485760 = 10MB)
block_sync_threshold_bytes = 10485760

# The block size in bytes used for calculating delta signatures. (e.g. 1048576 = 1MB)
block_size_bytes = 1048576

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
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "syncdir v{} — Windows background folder synchronization daemon",
            env!("CARGO_PKG_VERSION")
        );
        println!();
        println!("USAGE:");
        println!("    syncdir [OPTIONS]");
        println!();
        println!("OPTIONS:");
        println!("    --help, -h               Print this help message and exit");
        println!("    --version, -v            Print version and exit");
        println!(
            "    --register-startup       Register syncdir to start on Windows login and exit"
        );
        println!("    --unregister-startup     Remove syncdir from Windows startup and exit");
        println!();
        println!("When run without options, syncdir starts the background sync daemon.");
        std::process::exit(0);
    }

    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("syncdir {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    if args.iter().any(|a| a == "--register-startup") {
        match syncdir::startup::StartupRegistry::register() {
            Ok(()) => println!("Successfully registered syncdir for Windows startup."),
            Err(e) => {
                eprintln!("Failed to register startup: {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    if args.iter().any(|a| a == "--unregister-startup") {
        match syncdir::startup::StartupRegistry::unregister() {
            Ok(()) => println!("Successfully removed syncdir from Windows startup."),
            Err(e) => {
                eprintln!("Failed to unregister startup: {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    // Computes default app dir and sets up logging
    let app_dir = match Config::default_app_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Fatal error: {e}");
            std::process::exit(1);
        }
    };

    if !app_dir.exists()
        && let Err(e) = fs::create_dir_all(&app_dir)
    {
        eprintln!("Fatal error: Failed to create app directory: {e}");
        std::process::exit(1);
    }

    let log_dir = app_dir.join("logs");
    if !log_dir.exists()
        && let Err(e) = fs::create_dir_all(&log_dir)
    {
        eprintln!("Fatal error: Failed to create log directory: {e}");
        std::process::exit(1);
    }

    let file_appender = match Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("syncdir.log")
        .max_log_files(7)
        .build(&log_dir)
    {
        Ok(appender) => appender,
        Err(e) => {
            eprintln!("Fatal error: Failed to initialize log file writer: {e}");
            std::process::exit(1);
        }
    };

    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    let dual_writer = std::io::stdout.and(non_blocking);

    tracing_subscriber::fmt()
        .with_writer(dual_writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    if args.iter().any(|a| a == "--autostart") {
        tracing::info!("syncdir initialized (Trigger: Windows Auto-Start)");
    } else {
        tracing::info!("syncdir initialized (Trigger: Manual Launch)");
    }

    // Register panic hook to capture crash/panics
    std::panic::set_hook(Box::new(|panic_info| {
        let payload = panic_info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            *s
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.as_str()
        } else {
            "unknown panic payload"
        };
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        tracing::error!("Daemon panic at {location}: {message}");
        std::process::exit(1);
    }));

    if let Err(e) = try_main(app_dir) {
        tracing::error!("Fatal error: {e}");
        std::process::exit(1);
    }
}
