//! syncdir — A lightweight Windows user-session background sync utility with block-level delta synchronization.
//!
//! Mirrors a source folder to one or more destination folders using block-level
//! delta synchronization over the local network.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use syncdir::config::Config;
use syncdir::db::SqliteHashStore;
use syncdir::error::SyncError;
use syncdir::sync::{SyncCommand, start_sync_worker};
use syncdir::tray::{TrayExitReason, run_tray};
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::fmt::writer::MakeWriterExt;

fn try_main(app_dir: PathBuf) -> Result<TrayExitReason, SyncError> {
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
        return Ok(TrayExitReason::UserExit);
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
    let initial_source_online = config.source_dir.exists() && config.source_dir.is_dir();
    let source_online =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(initial_source_online));

    // 4. Initialize target databases and workers
    for (idx, dest) in dests.iter().enumerate() {
        let mut target_config = config.clone();
        target_config.dest_dir = Some(dest.clone());

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

        if dest.exists() && dest.is_dir() {
            tracing::info!(
                target_index = idx + 1,
                target_path = %dest.display(),
                "Target destination is online and reachable."
            );
        } else {
            tracing::warn!(
                target_index = idx + 1,
                target_path = %dest.display(),
                "Target destination is currently offline or unreachable."
            );
        }

        // Wire per-worker channel
        let (w_tx, w_rx) = channel();
        worker_txs.push(w_tx);

        tracing::info!(
            "Starting sync worker thread for target: {}...",
            dest.display()
        );
        let _worker_handle = start_sync_worker(
            idx,
            target_config,
            store,
            w_rx,
            Some(event_proxy.clone()),
            source_online.clone(),
        );
    }

    // 5. Central coordination channels and threads
    let (tx, rx) = channel();

    // Spawn central watcher coordinator thread
    let watcher_config = config.clone();
    let watcher_tx = tx.clone();
    let watcher_source_online = source_online.clone();
    let watcher_event_proxy = event_proxy.clone();
    std::thread::spawn(move || {
        let mut watcher: Option<syncdir::monitor::DirectoryWatcher> = None;
        let source_dir = watcher_config.source_dir.clone();
        let retry_interval = std::time::Duration::from_secs(watcher_config.retry_interval_seconds);
        let mut last_status_check = std::time::Instant::now()
            .checked_sub(retry_interval)
            .unwrap_or_else(std::time::Instant::now);

        let mut last_sent_online = None;
        let mut last_sent_active = None;

        loop {
            let now = std::time::Instant::now();

            if now.duration_since(last_status_check) >= retry_interval {
                last_status_check = now;
                let is_online = source_dir.exists() && source_dir.is_dir();
                watcher_source_online.store(is_online, std::sync::atomic::Ordering::Relaxed);

                let mut watcher_active = false;
                if is_online {
                    if watcher.is_none() {
                        tracing::info!("Source directory online. Starting directory watcher...");
                        match syncdir::monitor::DirectoryWatcher::start(
                            &watcher_config,
                            watcher_tx.clone(),
                        ) {
                            Ok(w) => {
                                watcher = Some(w);
                                watcher_active = true;
                            }
                            Err(e) => {
                                tracing::error!("Failed to start directory watcher: {e}");
                                watcher_active = false;
                            }
                        }
                    } else {
                        watcher_active = true;
                    }
                } else {
                    if watcher.is_some() {
                        tracing::warn!(
                            "Source directory went offline. Dropping directory watcher."
                        );
                        watcher = None;
                    }
                }

                if last_sent_online != Some(is_online) || last_sent_active != Some(watcher_active) {
                    last_sent_online = Some(is_online);
                    last_sent_active = Some(watcher_active);
                    let _ =
                        watcher_event_proxy.send_event(syncdir::tray::UserEvent::WatcherStatus {
                            source_online: is_online,
                            watcher_active,
                        });
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    // Spawn central broadcaster thread
    let broadcaster_rx = rx;
    let mut worker_senders = worker_txs;
    std::thread::spawn(move || {
        while let Ok(cmd) = broadcaster_rx.recv() {
            worker_senders.retain(|worker_tx| match worker_tx.send(cmd.clone()) {
                Ok(()) => true,
                Err(_) => {
                    tracing::warn!("Sync worker channel disconnected. Removing sender.");
                    false
                }
            });
        }
    });

    // Trigger initial sync scan
    let _ = tx.send(SyncCommand::TriggerFullScan);

    // 6. Run tray UI (blocks the main thread)
    tracing::info!("Starting system tray UI loop.");
    let initial_dest_online = dests.iter().map(|d| d.exists() && d.is_dir()).collect();
    let exit_reason = run_tray(
        event_loop,
        config_path,
        log_dir,
        tx,
        dests,
        initial_dest_online,
    )?;

    Ok(exit_reason)
}

/// RAII guard holding the single-instance Windows mutex handle.
///
/// Automatically closes the mutex handle via Win32 `CloseHandle` when dropped.
#[cfg(target_os = "windows")]
pub struct SingleInstanceGuard(*mut std::ffi::c_void);

#[cfg(target_os = "windows")]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: CloseHandle is a standard Win32 API function.
            unsafe {
                unsafe extern "system" {
                    fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
                }
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub struct SingleInstanceGuard;

/// Acquire a session-local named mutex to enforce single-instance execution.
///
/// Returns the mutex guard on success. The guard must be kept alive
/// for the lifetime of the process — dropping it releases the mutex.
/// Returns `None` if another instance already holds the mutex.
#[cfg(target_os = "windows")]
fn acquire_single_instance_mutex() -> Option<SingleInstanceGuard> {
    use std::os::windows::ffi::OsStrExt;
    // SAFETY: CreateMutexW and GetLastError are standard Win32 APIs called with a valid null-terminated wide string.
    unsafe {
        unsafe extern "system" {
            fn CreateMutexW(
                lp_mutex_attributes: *const std::ffi::c_void,
                b_initial_owner: i32,
                lp_name: *const u16,
            ) -> *mut std::ffi::c_void;
            fn GetLastError() -> u32;
            fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
        }
        const ERROR_ALREADY_EXISTS: u32 = 183;
        let name: Vec<u16> = std::ffi::OsStr::new("Local\\syncdir_single_instance\0")
            .encode_wide()
            .collect();
        let handle = CreateMutexW(std::ptr::null_mut(), 1, name.as_ptr());
        if handle.is_null() {
            return None;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return None;
        }
        Some(SingleInstanceGuard(handle))
    }
}

#[cfg(not(target_os = "windows"))]
fn acquire_single_instance_mutex() -> Option<SingleInstanceGuard> {
    Some(SingleInstanceGuard)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "syncdir v{} — Windows background folder synchronization daemon",
            env!("CARGO_PKG_VERSION")
        );
        println!("{}", syncdir::COPYRIGHT);
        println!("{}", env!("CARGO_PKG_REPOSITORY"));
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
        println!(
            "syncdir {} {}",
            env!("CARGO_PKG_VERSION"),
            syncdir::COPYRIGHT
        );
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

    // Enforce single-instance execution
    let _mutex_guard = match acquire_single_instance_mutex() {
        Some(handle) => handle,
        None => {
            eprintln!("syncdir is already running. Only one instance is allowed.");
            std::process::exit(0);
        }
    };

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

    let sys_info = syncdir::startup::SystemDiagnosticInfo::collect();
    tracing::info!(
        version = %sys_info.app_version,
        os = %sys_info.os_version,
        arch = %sys_info.arch,
        host = %sys_info.hostname,
        user = %sys_info.username,
        "System environment"
    );

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

    match try_main(app_dir) {
        Ok(exit_reason) => match exit_reason {
            TrayExitReason::UserExit => {
                tracing::info!("syncdir daemon shut down cleanly.");
            }
            TrayExitReason::Restart => {
                tracing::info!("Restarting syncdir daemon...");
                // Drop the mutex guard BEFORE spawning so the new instance can acquire it immediately.
                drop(_mutex_guard);
                if let Ok(exe) = std::env::current_exe() {
                    tracing::info!("Re-launching process: {}", exe.display());
                    let _ = std::process::Command::new(exe).spawn();
                }
            }
        },
        Err(e) => {
            tracing::error!("Fatal error: {e}");
            std::process::exit(1);
        }
    }
}
