//! syncdir — A lightweight Windows user-session background sync utility with block-level delta synchronization.
//!
//! Mirrors a source folder to one or more destination folders using block-level
//! delta synchronization over the local network.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use syncdir::config::Config;
use syncdir::daemon::{DaemonHandle, SyncDaemon};
use syncdir::error::SyncError;
use syncdir::net::{NetworkResolver, Win32NetworkResolver};
use syncdir::startup::RegistryBackend;
use syncdir::sync::ConnectivityState;
use syncdir::tray::{
    DestinationState, TrayActionHandler, TrayEventLoop, TrayExitReason, open_path,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static LOG_WORKER_GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);
static PANIC_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn try_main(app_dir: PathBuf) -> Result<TrayExitReason, SyncError> {
    let log_dir = app_dir.join("logs");
    tracing::info!("Initializing syncdir daemon...");

    // 1. Load or create configuration
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
    let tray_loop = TrayEventLoop::new()?;
    let observer = tray_loop.status_observer();

    let destinations: Vec<DestinationState> = config
        .resolved_dest_dirs()
        .into_iter()
        .map(|d| DestinationState::new(d, ConnectivityState::Offline))
        .collect();

    // Start daemon orchestrator
    let daemon = SyncDaemon::start(config, &app_dir, Some(observer))?;

    let handler = Arc::new(DaemonTrayHandler::new(
        config_path,
        log_dir,
        daemon.handle(),
        syncdir::startup::StartupRegistry,
    ));

    // Run tray UI (blocks the main thread)
    tracing::info!("Starting system tray UI loop.");
    let exit_reason = tray_loop.run(destinations, handler)?;

    daemon.shutdown();

    Ok(exit_reason)
}

/// Tray action handler connecting UI context menu callbacks to daemon and registry operations.
struct DaemonTrayHandler<R: RegistryBackend> {
    config_path: PathBuf,
    log_dir: PathBuf,
    handle: DaemonHandle,
    registry: R,
    resolver: Arc<dyn NetworkResolver>,
}

impl<R: RegistryBackend> DaemonTrayHandler<R> {
    /// Create a new tray handler with target config path, log directory, daemon handle, and registry backend.
    fn new(config_path: PathBuf, log_dir: PathBuf, handle: DaemonHandle, registry: R) -> Self {
        Self::with_resolver(
            config_path,
            log_dir,
            handle,
            registry,
            Arc::new(Win32NetworkResolver),
        )
    }

    /// Create a new tray handler with a custom network resolver.
    fn with_resolver(
        config_path: PathBuf,
        log_dir: PathBuf,
        handle: DaemonHandle,
        registry: R,
        resolver: Arc<dyn NetworkResolver>,
    ) -> Self {
        Self {
            config_path,
            log_dir,
            handle,
            registry,
            resolver,
        }
    }
}

impl<R: RegistryBackend + Send + Sync + 'static> TrayActionHandler for DaemonTrayHandler<R> {
    fn on_sync_now(&self) -> Result<(), SyncError> {
        self.handle.trigger_full_scan()
    }

    fn on_reload_config(&self) -> Result<(), SyncError> {
        let new_config = Config::load(&self.config_path)?;
        new_config.validate()?;
        SyncDaemon::validate_target_loops(&new_config, self.resolver.as_ref())?;
        Ok(())
    }

    fn on_toggle_startup(&self, enable: bool) -> Result<bool, SyncError> {
        if enable {
            self.registry.register()?;
            Ok(true)
        } else {
            self.registry.unregister()?;
            Ok(false)
        }
    }

    fn is_startup_enabled(&self) -> Result<bool, SyncError> {
        self.registry.is_registered()
    }

    fn on_open_config(&self) -> Result<(), SyncError> {
        open_path(&self.config_path).map_err(SyncError::Io)
    }

    fn on_view_logs(&self) -> Result<(), SyncError> {
        open_path(&self.log_dir).map_err(SyncError::Io)
    }
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

/// Structure containing OS and environment diagnostic information for troubleshooting.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemDiagnosticInfo {
    /// Operating system product name and build (e.g. "Windows 10 Pro 22H2 (Build 19045)")
    os_version: String,
    /// System architecture (e.g. "x86_64")
    arch: String,
    /// Computer hostname if available
    hostname: String,
    /// Current execution username if available
    username: String,
    /// Application version
    app_version: String,
}

impl SystemDiagnosticInfo {
    /// Queries the OS and environment to gather diagnostic details.
    ///
    /// This method is infallible and degrades gracefully with fallback strings if registry
    /// or environment variable queries fail.
    fn collect() -> Self {
        #[cfg(windows)]
        let os_version = {
            use winreg::RegKey;
            use winreg::enums::HKEY_LOCAL_MACHINE;
            let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
            if let Ok(key) = hklm.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
                let product_name: String = key
                    .get_value("ProductName")
                    .unwrap_or_else(|_| "Windows".to_string());
                let display_version: String = key
                    .get_value("DisplayVersion")
                    .or_else(|_| key.get_value("ReleaseId"))
                    .unwrap_or_default();
                let build_number: String = key.get_value("CurrentBuildNumber").unwrap_or_default();

                let mut version_str = product_name;
                if !display_version.is_empty() {
                    version_str.push(' ');
                    version_str.push_str(&display_version);
                }
                if !build_number.is_empty() {
                    version_str.push_str(&format!(" (Build {build_number})"));
                }
                version_str
            } else {
                format!("Windows ({})", std::env::consts::ARCH)
            }
        };

        #[cfg(not(windows))]
        let os_version = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);

        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "UnknownHost".to_string());

        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "UnknownUser".to_string());

        Self {
            os_version,
            arch: std::env::consts::ARCH.to_string(),
            hostname,
            username,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

fn pseudonymize_identifier(raw: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("syncdir telemetry pseudonymization v1");
    hasher.update(raw.as_bytes());
    let hash = hasher.finalize();
    let hex_prefix = &hash.to_hex()[..12];
    format!("anon-{}", hex_prefix)
}

fn log_system_environment(sys_info: &SystemDiagnosticInfo) {
    let anon_user = pseudonymize_identifier(&sys_info.username);
    let anon_host = pseudonymize_identifier(&sys_info.hostname);
    tracing::info!(
        version = %sys_info.app_version,
        os = %sys_info.os_version,
        arch = %sys_info.arch,
        anon_user = %anon_user,
        anon_host = %anon_host,
        "System diagnostic environment information"
    );
}

fn write_emergency_panic_log(log_dir: &std::path::Path, location: &str, message: &str) {
    let crash_file = log_dir.join("crash.log");
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let report = format!("[{timestamp}] PANIC at {location}: {message}\n");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&crash_file)
    {
        use std::io::Write;
        let _ = file.write_all(report.as_bytes());
        let _ = file.flush();
    }
}

fn handle_panic_diagnostic_core(log_dir: &std::path::Path, location: &str, message: &str) {
    eprintln!("Daemon panic at {location}: {message}");
    write_emergency_panic_log(log_dir, location, message);
    tracing::error!("Daemon panic at {location}: {message}");
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

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    if let Ok(mut g) = LOG_WORKER_GUARD.lock() {
        *g = Some(guard);
    }

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(false);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    let sys_info = SystemDiagnosticInfo::collect();
    let anon_host = pseudonymize_identifier(&sys_info.hostname);
    let _host_span = tracing::info_span!("syncdir", host = %anon_host).entered();
    log_system_environment(&sys_info);

    if args.iter().any(|a| a == "--autostart") {
        tracing::info!("syncdir initialized (Trigger: Windows Auto-Start)");
    } else {
        tracing::info!("syncdir initialized (Trigger: Manual Launch)");
    }

    // Register panic hook to capture crash/panics
    let log_dir_for_panic = log_dir.clone();
    std::panic::set_hook(Box::new(move |panic_info| {
        if PANIC_IN_PROGRESS.swap(true, Ordering::SeqCst) {
            eprintln!("Double panic detected in syncdir; aborting immediately.");
            std::process::exit(1);
        }

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

        handle_panic_diagnostic_core(&log_dir_for_panic, &location, message);

        // Prevent self-join deadlock if the logger thread itself panics
        let is_logger_thread = std::thread::current().name() == Some("tracing-appender");
        if !is_logger_thread
            && let Ok(mut guard_slot) = LOG_WORKER_GUARD.try_lock()
            && let Some(guard) = guard_slot.take()
        {
            drop(guard); // Flushes queued logs to disk
        }

        std::process::exit(1);
    }));

    match try_main(app_dir) {
        Ok(exit_reason) => match exit_reason {
            TrayExitReason::UserExit => {
                tracing::info!("syncdir daemon shut down cleanly.");
                if let Ok(mut guard_slot) = LOG_WORKER_GUARD.lock() {
                    let _ = guard_slot.take();
                }
            }
            TrayExitReason::Restart => {
                tracing::info!("Restarting syncdir daemon...");
                if let Ok(mut guard_slot) = LOG_WORKER_GUARD.lock() {
                    let _ = guard_slot.take();
                }
                // Drop the mutex guard BEFORE spawning so the new instance can acquire it immediately.
                drop(_mutex_guard);
                if let Ok(exe) = std::env::current_exe() {
                    tracing::info!("Re-launching process: {}", exe.display());
                    if let Err(e) = std::process::Command::new(&exe).spawn() {
                        tracing::error!(
                            error = %e,
                            exe = %exe.display(),
                            "Failed to re-launch syncdir process"
                        );
                    }
                }
            }
        },
        Err(e) => {
            tracing::error!("Fatal error: {e}");
            if let Ok(mut guard_slot) = LOG_WORKER_GUARD.lock() {
                let _ = guard_slot.take();
            }
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use syncdir::startup::MockStartupRegistry;
    use syncdir::sync::SyncCommand;
    use syncdir::tray::TrayActionHandler;

    #[test]
    fn test_sync_daemon_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = syncdir::config::Config::test_default(src, dst);
        let daemon = SyncDaemon::start(config, temp.path(), None).expect("daemon start");
        assert!(daemon.config().retry_interval_seconds() > 0);
        daemon.shutdown();
    }

    #[test]
    fn test_daemon_tray_handler() {
        let (tx, rx) = channel();
        let mock_registry = MockStartupRegistry::new(false);
        let handle = syncdir::daemon::DaemonHandle::new(tx);
        let handler = DaemonTrayHandler::new(
            PathBuf::from("config.toml"),
            PathBuf::from("logs"),
            handle,
            mock_registry,
        );

        assert!(!handler.is_startup_enabled().unwrap());
        assert!(handler.on_toggle_startup(true).unwrap());
        assert!(handler.is_startup_enabled().unwrap());
        assert!(!handler.on_toggle_startup(false).unwrap());
        assert!(!handler.is_startup_enabled().unwrap());

        handler.on_sync_now().unwrap();
        assert_eq!(rx.recv().unwrap(), SyncCommand::TriggerFullScan);
    }

    #[test]
    fn test_daemon_tray_handler_actions() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
source_dir = "C:\\dummy_source"
dest_dir = "C:\\dummy_dest"
debounce_seconds = 3
propagate_deletions = true
block_sync_threshold_bytes = 10485760
block_size_bytes = 1048576
verify_writes = true
"#,
        )
        .unwrap();

        let (tx, rx) = channel();
        let mock_registry = MockStartupRegistry::new(false);
        let handle = DaemonHandle::new(tx);
        let handler = DaemonTrayHandler::new(
            config_path,
            dir.path().join("logs"),
            handle.clone(),
            mock_registry,
        );

        assert!(!handler.is_startup_enabled().unwrap());
        assert!(handler.on_toggle_startup(true).unwrap());
        assert!(handler.is_startup_enabled().unwrap());
        assert!(!handler.on_toggle_startup(false).unwrap());
        assert!(!handler.is_startup_enabled().unwrap());

        handler.on_sync_now().unwrap();
        let cmd = rx.try_recv().unwrap();
        assert_eq!(cmd, SyncCommand::TriggerFullScan);

        // Valid reload
        assert!(handler.on_reload_config().is_ok());

        // Recursive loop reload should fail
        let loop_config_path = dir.path().join("loop_config.toml");
        std::fs::write(
            &loop_config_path,
            r#"
source_dir = "C:\\dummy_source"
dest_dir = "C:\\dummy_source\\nested"
debounce_seconds = 3
propagate_deletions = true
block_sync_threshold_bytes = 10485760
block_size_bytes = 1048576
verify_writes = true
"#,
        )
        .unwrap();
        let loop_handler = DaemonTrayHandler::new(
            loop_config_path,
            dir.path().join("logs"),
            handle,
            MockStartupRegistry::new(false),
        );
        let reload_err = loop_handler.on_reload_config().unwrap_err();
        assert!(
            reload_err.to_string().contains("recursive sync loop"),
            "Expected recursive sync loop error, got: {}",
            reload_err
        );
    }

    #[test]
    fn test_system_diagnostic_info_collect() {
        let info = SystemDiagnosticInfo::collect();
        assert!(
            !info.os_version.is_empty(),
            "os_version should not be empty"
        );
        assert!(!info.arch.is_empty(), "arch should not be empty");
        assert!(!info.hostname.is_empty(), "hostname should not be empty");
        assert!(!info.username.is_empty(), "username should not be empty");
        assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_anonymize_username_variants() {
        let user1 = "alice";
        let user2 = "bob";
        let anon1 = pseudonymize_identifier(user1);
        let anon2 = pseudonymize_identifier(user2);

        assert!(!anon1.contains("alice"));
        assert!(!anon2.contains("bob"));
        assert!(anon1.starts_with("anon-"));
        assert_eq!(
            anon1,
            pseudonymize_identifier(user1),
            "Pseudonymization must be deterministic"
        );
        assert_ne!(
            anon1, anon2,
            "Different usernames must yield different pseudonyms"
        );
    }

    #[test]
    fn test_system_environment_log_does_not_expose_raw_username() {
        let raw_user = "SecretAdminUser123";
        let raw_host = "ConfidentialHost456";
        let sys_info = SystemDiagnosticInfo {
            os_version: "Windows 11 Pro".to_string(),
            arch: "x86_64".to_string(),
            hostname: raw_host.to_string(),
            username: raw_user.to_string(),
            app_version: "0.1.13".to_string(),
        };

        let (_result, log_output) = syncdir::test_support::with_captured_tracing(|| {
            log_system_environment(&sys_info);
        });

        assert!(
            !log_output.contains(raw_user),
            "Plaintext username must not be logged"
        );
        assert!(
            !log_output.contains(raw_host),
            "Plaintext hostname must not be logged"
        );
        assert!(
            log_output.contains("anon-"),
            "Pseudonymized identifier prefix must be present"
        );
    }

    #[test]
    fn test_panic_hook_flushes_diagnostics_without_buffer_loss() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let location = "src/sync/worker.rs:125:10";
        let message = "simulated worker thread critical panic";

        let (_result, log_output) = syncdir::test_support::with_captured_tracing(|| {
            handle_panic_diagnostic_core(&log_dir, location, message);
        });

        assert!(log_output.contains("Daemon panic at src/sync/worker.rs:125:10"));
        assert!(log_output.contains("simulated worker thread critical panic"));

        let emergency_crash_log = log_dir.join("crash.log");
        assert!(
            emergency_crash_log.exists(),
            "crash.log must be created as emergency fallback"
        );
        let crash_content = std::fs::read_to_string(&emergency_crash_log).unwrap();
        assert!(crash_content.contains(location));
        assert!(crash_content.contains(message));
    }
}
