# Project Context: syncdir

This file documents the chronological history, design decisions, and rules context for the development of `syncdir`.

## 1. Active Architecture Decisions

* **Language Choice (Rust)**: Selected Rust for compiling to a single high-performance binary, minimal memory and CPU usage (crucial for a daemon/tray process), and native bindings to Windows APIs.
* **Execution Model (User Startup Process)**: Chose running in the user session over a system service. This ensures the sync process has identical permissions to the user and can directly access network shares/mapped drives (e.g. `Z:\`), which are scoped to active user sessions.
* **Delta Synchronization**:
  * **Small Files (<10MB)**: Full overwrite on changes.
  * **Large Files (>=10MB)**: In-place block overwrite. Split files into 1MB blocks, calculate Blake3 signatures, compare against local signature database, seek and overwrite changed blocks in-place on the target.
  * **Signature Cache**: Use local SQLite database to store hashes. This avoids reading/downloading target files over SMB to compute hashes, preserving network bandwidth.
* **Real-time Change Detection**: Hybrid model. Use `ReadDirectoryChangesW` (via `notify` crate) with a 3-second write debounce. Run a full scan on application startup and periodically to ensure eventually consistent alignment.
* **UI & Configuration**:
  * Run as a silent windowless tray icon in the notification area.
  * Configuration in `config.toml` under `%APPDATA%\syncdir\config.toml`.
  * Tray Menu: Open Config, View Logs, Sync Now, Start on System Startup (Checkable), Exit.
* **Startup Registry & Headless CLI Options**:
  * Chose Windows Registry integration (`StartupRegistry`) under HKCU Run key (`Software\Microsoft\Windows\CurrentVersion\Run`) for user login startup.
  * Provided early-exit native CLI arguments (`--register-startup`, `--unregister-startup`, `--help`, `--version`) that print to stdout/stderr and exit immediately.
  * Auto-start trigger detection: Log distinct telemetry when running with `--autostart` flag.
  * UI Integration: Checkbox in tray menu syncs state with registry, with fallback restoration if registry writes fail.
* **Conflict & Deletion Strategy**:
  * One-way synchronization (source is source of truth).
  * Source deletions are propagated to destination (with `propagate_deletions = true` default).
  * Overwritten or deleted files on destination are moved to `<dest>/.syncdir_archive/<timestamp>_<path>` to prevent accidental data loss. Reversion is manual (user moves files back to source folder).
* **Write-Verification & Integrity**:
  * Implement **Write-Verify (Block Only)** logic when `verify_writes = true` in configuration. Rewritten 1MB chunks on the target are read back immediately and their Blake3 hashes validated.
  * Destination files' last-modified timestamps are explicitly aligned with source file metadata at completion of sync to allow metadata-only fast-path verification.
  * SQLite caches check active `block_size_bytes` and `block_sync_threshold_bytes` configuration parameters; any drift invalidates the database cache, forcing a safe rebuild.

## 2. Chronological History

* **2026-07-14**: Workspace initialized. Run `/toolcheck` to verify environment toolchains (Rustc, Cargo, Git, MSVC Linker, ripgrep, ast-grep).
* **2026-07-14**: Alignment on key design options using the `/grill-me` process. Established tech stack, execution model, delta sync mechanism, change detection, and deletion archiving strategy.
* **2026-07-14**: Architect created core documentation: `architecture.md`, `spec.md`, and `context.md`.
* **2026-07-14**: Architect reviewed architecture, identifying SQLite FK cascade issues, log folder setup, and configuration drift. Updated plans and specs.
* **2026-07-14**: Brainstormed verification strategies. Decided on Block-only write-verification, timestamp alignment, and the `verify_writes` configuration setting. Updated plans, specs, and architecture.
* **2026-07-14**: Builder successfully completed Phase 1 implementation (Project Foundation & Core Infrastructure). Created Cargo dependencies, core `SyncError` definitions, validated `Config` structures with tests, defined the `SyncCommand` channel-passing and `SyncEngine` interfaces, and implemented a robust `SqliteHashStore` for signature caching with automated cascades and configuration invalidation. Verified with 7 passing tests and zero clippy/fmt compiler alerts.
* **2026-07-14**: Builder successfully completed Phase 2 implementation (Delta Sync Engine & Directory Monitoring). Designed and implemented the in-place block-level delta sync engine, real-time filesystem watcher using `notify` (wrapping Windows directory notifications), background worker with debouncing, database deletion list helpers, and comprehensive test suite containing unit tests for delta sync / deletion archiving and end-to-end integration tests. Verified with 12 passing tests and clean clippy / formatter verification.
* **2026-07-14**: Builder successfully completed Phase 3 implementation (System Tray UI, Tracing, and Application Wiring). Configured a non-blocking dual-appender tracing system writing daily to `%APPDATA%\syncdir\logs` and standard output. Integrated a windowless system tray interface using `winit` v0.29 event loops and `tray-icon` menus. Added configuration autoload/auto-create patterns for seamless user experience. Verified with 15 passing tests and zero clippy/fmt compiler warnings.
* **2026-07-14**: Builder successfully completed Phase 4 implementation (Startup Registry Integration & Logging). Integrated `winreg` crate to read/write the HKCU `Software\Microsoft\Windows\CurrentVersion\Run` key with `--autostart` suffix. Added native argument parser in `main()` supporting early-exit flags and auto-start detection logging. Embedded a checkable startup option in the system tray UI with fallbacks. Verified with 16 passing tests and clean format/clippy checks.
* **2026-07-14**: Builder addressed qualitative code review findings: refactored `StartupRegistry` and its Windows/non-Windows conditional implementations from `src/config.rs` into a dedicated high-cohesion `src/startup.rs` module, and updated the global panic hook in `src/main.rs` to invoke `std::process::exit(1)` upon crash detection to prevent background thread panics from leaving the daemon in a silent zombie state. Verified with 16 passing tests and zero clippy/fmt errors.
* **2026-07-14**: Builder remediated audit findings from the previous phase: added top-level module documentation comments to `src/startup.rs`, and restored the missing `build-report` template in `.agents/rules/builder-rules.md` to resolve circular references in documentation. Verified with 16 passing tests and zero clippy/fmt errors.
* **2026-07-14**: Builder successfully completed Phase 6 implementation (Rename Event Handling in Directory Watcher). Modified `DirectoryWatcher`'s event callback in `src/monitor.rs` to explicitly match and process `ModifyKind::Name(RenameMode)` events. Implemented support for pairing `RenameMode::Both`, `From`, and `To` occurrences into corresponding `SyncCommand::FileDeleted` (for the old path) and `SyncCommand::FileModified` (for the new path) messages. Added `test_watcher_rename_event` to verify functionality. Verified with 18 passing tests and zero clippy/fmt errors.
* **2026-07-14**: Architect synchronized documentation. Updated `spec.md` behavioral contracts and validation hashes to reflect DirectoryWatcher API and rename scenarios. Added comprehensive rustdoc comments and runnable doc-tests to `DirectoryWatcher::start` in `src/monitor.rs` ensuring zero-drift and full compliance.
* **2026-07-14**: Builder addressed qualitative review findings: refactored SQLite connection locking with centralized `conn(&self)` helper in `src/db.rs`, mapped mutex poison errors to a new `SyncError::LockPoison` variant, implemented safe relative path checks (`is_safe_relative_path`) at all entry points of `SyncEngine` (`src/sync.rs`), upgraded signature cache to version 2 metadata checking (`db_version = "2"`) with automatic migration purges, and aligned `main.rs` default config template with the standard specifications. Verified with 21 passing tests and zero clippy/fmt errors.
* **2026-07-14**: Builder successfully published the project to GitHub using the `gh` CLI. Created the public repository `wends155/syncdir` at `https://github.com/wends155/syncdir`, generated the standard MIT `LICENSE` file under copyright Wendell Saligan, configured `Cargo.toml` with `license = "MIT"` metadata, updated `README.md` clone endpoints, renamed default local execution branch to `main`, and pushed the entire workspace commits.
* **2026-07-14**: Builder bumped version to `0.1.1` in `Cargo.toml`, built the binary in production release mode (`cargo build --release`), and created the official GitHub release `v0.1.1` with the downloadable Windows daemon artifact `syncdir.exe` attached at `https://github.com/wends155/syncdir/releases/tag/v0.1.1`.
* **2026-07-14**: Builder successfully implemented robust directory presence checks, configurable polling/retry loops, and status tray signaling. Added `retry_interval_seconds` field (default 10) to `Config` allowing soft warnings on missing directories at boot time. Wired winit `EventLoop` in `main.rs` to pass `EventLoopProxy` to the background sync worker. Refactored the sync worker to dynamically manage directory watcher instances (stopping when source goes offline, starting when online) and to skip and debounce file synchronization operations if mount points are disconnected. Added winit `UserEvent` to tray menu loop to dynamically paint color-coded status icons (Healthy/Blue, Source Offline/Red, Destination Offline/Yellow, Both Offline/Gray) and update tooltip telemetry. Added empty source safety threshold in `run_full_scan` to prevent accidental deletion propagation if network drives unmount. Verified with 24 passing tests and zero clippy/fmt errors.
* **2026-07-14**: Builder resolved qualitative code review findings in `src/sync.rs`. Hoisted `source_online` and `dest_online` status variables inside the background thread to eliminate redundant `.exists()` filesystem checks in queue loops, preventing latency spikes on disconnected network shares. Replaced raw `Instant::now()` subtraction with `checked_sub` and a fallback mechanism to prevent thread panics if the daemon is launched within the polling window threshold of Windows system startup. Verified with 24 passing tests and zero clippy/fmt warnings.
* **2026-07-14**: Architect executed `/update-doc` workflow. Synchronized [spec.md](file:///c:/Users/WSALIGAN/code/syncdir/spec.md) behavioral contracts with the new presence recovery, status signaling event loop, and empty directory safety thresholds. Enriched rustdoc comments in `src/tray.rs` and `src/sync.rs` with Arguments, Returns, and Errors sections to fulfill high-coverage requirements. Verified correctness of all doc-tests and registered source commit `09bf1e0` as the verified metadata baseline.





