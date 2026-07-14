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

# The primary destination directory to synchronize changes to.
dest_dir = "C:\\path\\to\\destination"

# Optional additional destination directories to synchronize changes to.
# dest_dirs = [
#     "D:\\backup\\destination1",
#     "E:\\backup\\destination2"
# ]

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

# The retry interval in seconds when directories are offline.
retry_interval_seconds = 10
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

    // Initialize winit event loop on main thread before creating threads
    let event_loop =
        winit::event_loop::EventLoopBuilder::<syncdir::tray::UserEvent>::with_user_event()
            .build()
            .map_err(|e| SyncError::Tray(format!("Failed to create event loop: {e}")))?;
    let event_proxy = event_loop.create_proxy();

    let dests = config.resolved_dest_dirs();
    let mut worker_txs = Vec::new();

    // 4. Initialize target databases and workers
    for (idx, dest) in dests.iter().enumerate() {
        let mut target_config = config.clone();
        target_config.dest_dir = dest.clone();

        // Calculate isolated SQLite database filename using Blake3 hash of the target path
        let dest_str = dest.to_string_lossy();
        let hash = blake3::hash(dest_str.as_bytes());
        let db_filename = format!("sigcache_{}.db", hash.to_hex());
        let db_path = app_dir.join(db_filename);

        tracing::info!(
            "Opening signature cache database for target {} at: {}",
            dest.display(),
            db_path.display()
        );
        let store = SqliteHashStore::new(&db_path, &target_config)?;

        // Wire per-worker channel
        let (w_tx, w_rx) = channel();
        worker_txs.push(w_tx);

        tracing::info!(
            "Starting sync worker thread for target: {}...",
            dest.display()
        );
        let _worker_handle =
            start_sync_worker(idx, target_config, store, w_rx, Some(event_proxy.clone()));
    }

    // 5. Central coordination channels and threads
    let (tx, rx) = channel();

    // Spawn central watcher coordinator thread
    let watcher_config = config.clone();
    let watcher_tx = tx.clone();
    std::thread::spawn(move || {
        let mut watcher: Option<syncdir::monitor::DirectoryWatcher> = None;
        let source_dir = watcher_config.source_dir.clone();
        let retry_interval = std::time::Duration::from_secs(watcher_config.retry_interval_seconds);
        let mut last_status_check = std::time::Instant::now()
            .checked_sub(retry_interval)
            .unwrap_or_else(std::time::Instant::now);

        loop {
            let now = std::time::Instant::now();

            if now.duration_since(last_status_check) >= retry_interval {
                last_status_check = now;
                let source_online = source_dir.exists() && source_dir.is_dir();

                if source_online {
                    if watcher.is_none() {
                        tracing::info!("Source directory online. Starting directory watcher...");
                        match syncdir::monitor::DirectoryWatcher::start(
                            &watcher_config,
                            watcher_tx.clone(),
                        ) {
                            Ok(w) => {
                                watcher = Some(w);
                            }
                            Err(e) => {
                                tracing::error!("Failed to start directory watcher: {e}");
                            }
                        }
                    }
                } else {
                    if watcher.is_some() {
                        tracing::warn!(
                            "Source directory went offline. Dropping directory watcher."
                        );
                        watcher = None;
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    // Spawn central broadcaster thread
    let broadcaster_rx = rx;
    let worker_senders = worker_txs;
    std::thread::spawn(move || {
        while let Ok(cmd) = broadcaster_rx.recv() {
            for worker_tx in &worker_senders {
                let _ = worker_tx.send(cmd.clone());
            }
        }
    });

    // Trigger initial sync scan
    let _ = tx.send(SyncCommand::TriggerFullScan);

    // 6. Run tray UI (blocks the main thread)
    tracing::info!("Starting system tray UI loop.");
    run_tray(event_loop, config_path, log_dir, tx, dests)?;

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
