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
* **Multiple Destinations (Broadcaster Model)**: Enabled backing up a single source to multiple target folders by separating directory watching from sync workers. A central coordinator runs the file monitor and broadcasts detected path changes to a set of independent, isolated workers, each using its own dedicated SQLite signature cache database named with the target path's Blake3 hash.
* **Aggregated Health Status UI**: The tray icon reflects the overall state of all configured backups: Blue if all targets are online, Yellow if some targets are offline, and Red/Gray if the source folder is missing. The tray menu displays each destination directory with check/uncheck indicators showing its specific active status.
* **Strongly-Typed Path Domain Types (`TargetDir` & `DestinationCollection`)**: Consolidated path normalization (forward slashes to backslashes, drive root trailing backslash `R:\`, single-backslash UNC repair `\\172...`, redundant trailing slash pruning) and destination management into strongly-typed domain abstractions. `TargetDir` encapsulates self-normalizing path invariants and role-based format validation (`"source"` vs `"destination"`). `DestinationCollection` encapsulates ordered destination lists with Windows case-insensitive deduplication. Transparent backward compatibility with legacy TOML configurations is handled via a private `RawConfig` Serde bridge (`#[serde(from = "RawConfig", into = "RawConfig")]`), while `ConfigBuilder` remains infallible with validation deferred to `Config::validate()`.

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
* **2026-07-14**: Builder resolved issue where unescaped Windows single backslashes in double-quoted paths of `config.toml` caused TOML parsing escape sequence crashes. Implemented a robust TOML pre-processor helper in `src/config.rs` that automatically doubles raw single backslashes in path keys while leaving valid double backslashes intact. Added a new unit test verifying this functionality. Verified with 25 passing tests and clean clippy / format checks.
* **2026-07-14**: Builder successfully implemented the "Multiple Destinations for a single source" feature. Extended `Config` with `dest_dirs`, updated the TOML preprocessor for array format parsing, isolated SQLite block caches by naming files using target path Blake3 hashes, launched separate sync workers per target, moved filesystem monitoring to a central watcher thread with a broadcast broker channel, and updated the Windows tray icon and tooltip to dynamically reflect aggregated health states (Blue/Yellow/Red/Gray) and individual target statuses. Verified with 26/26 passing tests and clean format/clippy audits.
* **2026-07-14**: Builder successfully resolved watcher failure and status telemetry findings. Removed the conflicting `source_online` field from `TargetStatusUpdate`, letting the central watcher coordinator exclusively own source presence state via `UserEvent::WatcherStatus` messages. Introduced a thread-safe `Arc<AtomicBool>` source status flag shared between the coordinator and workers to eliminate filesystem checks in worker loop queues. Unified tray repainting into a single deferred block to prevent redundant redraws, and updated the central broadcaster to dynamically retain active worker senders while removing disconnected channels. Resolved a Config initialization compilation error in the `DirectoryWatcher::start` doc-test. Verified with 26/26 passing tests (plus doc-tests) and clean Clippy/format audits.
* **2026-07-14**: Architect synchronized documentation and spec contracts. Updated `spec.md` with new `UserEvent::WatcherStatus` enum structure, revised `start_sync_worker` signatures, and recorded verified commit `64c477d` as the baseline. Enriched rustdoc comments in `src/sync.rs` for `start_sync_worker` parameters.
* **2026-07-14**: Builder successfully executed an S-Tier plan to update and correct outdated details in `architecture.md`. Rewrote Overview, Key Features, Module Boundaries, Diagrams, Known Constraints, and Environment Configuration sections to accurately reflect the multi-destination coordinator-broadcaster engine, namespaced SQLite cache databases, and tray status UI details. Checked with 27 passing tests.
* **2026-07-15**: Builder successfully configured `ast-grep` (`sg`) with project-specific static analysis rules for Rust code safety, SQL injection prevention, path traversal auditing, and scattered environment variable centralization. Verified rule functionality and verified zero-exit on linter, formatter, and test suites.
* **2026-07-25**: Builder updated `.ast-grep/rules/path-traversal-leak.yml` to include `fs::remove_file` and `fs::remove_dir_all` deletion patterns per review findings. Verified rule detection and verified zero-exit on linter, formatter, and test suites.
* **2026-07-25**: Architect executed `/update-doc` workflow. Verified zero drift across package descriptions, updated `spec.md` verification commit hash to `cafe65f`, and validated doc-tests.
* **2026-07-25**: Builder updated `architecture.md` to document `startup` module boundaries and dependency direction rules per architecture audit findings.
* **2026-07-25**: Builder enriched `Cargo.toml` with crates.io package metadata (`repository`, `homepage`, `documentation`, `readme`, `keywords`, `categories`) and bumped version to `0.1.4`. Verified zero-warning `cargo publish --dry-run`.
* **2026-07-25**: Builder created `CHANGELOG.md`, compiled release binary `syncdir.exe`, pushed `v0.1.4` git tag, and published GitHub Release `v0.1.4` at `https://github.com/wends155/syncdir/releases/tag/v0.1.4`.
* **2026-07-25**: Builder implemented `MockHashStore` in `src/db.rs`, `RegistryBackend` trait abstraction in `src/startup.rs`, and expanded unit test suite in `src/sync.rs` with 0-byte file, exact block multiple, and worker queue storm debouncing tests (24/24 passing unit tests).
* **2026-07-25**: Builder replaced raw LaTeX math delimiters (`$\ge$`) with standard UTF-8 characters (`≥`) in `CHANGELOG.md`, `README.md`, and GitHub Release `v0.1.4` notes to fix browser Markdown rendering defects.
* **2026-07-25**: Builder made `dest_dir` optional (`Option<PathBuf>`) in `Config`, allowing multi-destination configurations specified solely via `dest_dirs`. Updated `resolved_dest_dirs()`, `validate()`, engine/worker callers, doc-tests, and test helpers across 6 files (26/26 passing unit tests).
* **2026-07-25**: Architect executed `/update-doc` workflow. Synchronized rustdoc comments in `src/startup.rs`, updated `spec.md` behavioral contracts and verification commit hash to `4092fd4`, and confirmed zero drift across `Cargo.toml`, `lib.rs`, and `README.md`.
* **2026-07-25**: Builder updated `architecture.md` § 5 Module Boundaries to accurately reflect `MockHashStore`, `RegistryBackend`, `MockStartupRegistry`, and optional `dest_dir` configuration support.
* **2026-07-25**: Builder bumped version to `0.1.5`, updated `CHANGELOG.md`, compiled release binary `syncdir.exe`, pushed `v0.1.5` git tag, verified `cargo publish --dry-run`, and published GitHub Release `v0.1.5` at `https://github.com/wends155/syncdir/releases/tag/v0.1.5`.
* **2026-07-25**: Builder added `authors = ["Wendell Saligan"]` to `Cargo.toml`, exported `COPYRIGHT` in `src/lib.rs`, updated `--help` and `--version` CLI output in `src/main.rs`, and added an "About" menu item in `src/tray.rs` triggering a native Windows `MessageBoxW` modal dialog (34/34 passing tests).
* **2026-07-25**: Architect executed `/update-doc` workflow. Synchronized `spec.md` behavioral contracts and verification commit hash to `3de93a5`, and confirmed zero metadata drift across `Cargo.toml`, `lib.rs`, and `README.md`.
* **2026-07-25**: Builder bumped version to `0.1.6`, updated `CHANGELOG.md`, compiled release binary `syncdir.exe`, pushed `v0.1.6` git tag, verified `cargo publish --dry-run`, and published GitHub Release `v0.1.6` at `https://github.com/wends155/syncdir/releases/tag/v0.1.6`.
* **2026-07-25**: Builder fixed startup initial scan race condition by initializing `source_online` atomic from live directory check in `src/main.rs`, initializing worker local `source_online` from atomic, and evaluating live filesystem presence in `TriggerFullScan` in `src/sync.rs` (34/34 passing tests).
* **2026-07-25**: Builder added "Reload Config" tray menu option in `src/tray.rs` with native Windows `show_error_dialog` (`MessageBoxW`) validation feedback and clean process re-launch via `restart_process` (34/34 passing tests).
* **2026-07-25**: Builder addressed meta-review findings by adding `Config::test_default()` helper, refactoring test config boilerplate, adding 3 unit/integration tests for offline TriggerFullScan, Reload Config validation error, and nested deletion archive, and documenting `RegistryBackend` design debt (37/37 passing tests).
* **2026-07-25**: Architect executed `/update-doc` workflow. Synchronized `spec.md` behavioral contracts and verification commit hash to `35f8f80`, and confirmed zero metadata drift across `Cargo.toml`, `lib.rs`, and `README.md`.
* **2026-07-25**: Builder updated `architecture.md` Sections 2, 5, 10, 14 per Architecture Recommendations Report to document Reload Config tray menu features, process restart capabilities, `Config::test_default()` shared test helper, and `RegistryBackend` static call decoupling debt.
* **2026-07-25**: Builder bumped version to `0.1.7`, updated `CHANGELOG.md`, compiled release binary `syncdir.exe`, pushed `v0.1.7` git tag, verified `cargo publish --dry-run`, and published GitHub Release `v0.1.7` at `https://github.com/wends155/syncdir/releases/tag/v0.1.7` (37/37 passing tests).
* **2026-07-25**: Builder updated `run_full_scan()` in `src/sync.rs` to handle file sync and deletion errors gracefully with `tracing::warn!` logging and skip-count summaries instead of early-aborting on unmapped/offline destination paths (38/38 passing tests).
* **2026-07-25**: Builder added `SystemDiagnosticInfo::collect()` in `src/startup.rs` and structured startup logging in `src/main.rs`, logging OS version/edition, build number, arch, hostname, username, and app version (39/39 passing tests).
* **2026-07-25**: Builder synchronized Tray UI status telemetry with worker full scan write failures by returning `Ok(false)` on 100% file sync failure and notifying tray loop to update icon to yellow/DestinationOffline (40/40 passing tests).
* **2026-07-25**: Builder added `tracing::info!` file copy telemetry logging and SMB ±2000 ms destination timestamp tolerance in `src/sync.rs` for Windows network share synchronization (41/41 passing tests).
* **2026-07-25**: Builder implemented UNC path TOML escaping, defensive `\172...` -> `\\172...` normalization, strict path format validation in `src/config.rs`, and startup target reachability logging in `src/main.rs` (44/44 passing tests).
* **2026-07-25**: Architect synchronized `spec.md` behavioral contracts with commit `d1b4b44`, adding UNC path normalization, strict path validation, SMB 2-second timestamp tolerance, and file telemetry logging scenarios.
* **2026-07-25**: Builder synchronized `architecture.md` technical design (Key Features §2, Module Boundaries §5, Observability & Logging §9) with recent UNC path normalization, SMB timestamp tolerance, file telemetry logging, and startup reachability features.
* **2026-07-25**: Builder bumped version to `0.1.8`, updated `CHANGELOG.md`, compiled release binary `syncdir.exe`, pushed `v0.1.8` git tag, verified `cargo publish --dry-run`, and published GitHub Release `v0.1.8` at `https://github.com/wends155/syncdir/releases/tag/v0.1.8` (44/44 passing tests).
* **2026-07-30**: Builder implemented single-instance Win32 named mutex (`Local\syncdir_single_instance`) in `src/main.rs` with `SingleInstanceGuard` RAII handle, and refactored process restart in `src/tray.rs` to return `TrayExitReason` enum from `run_tray` so `TrayIcon::Drop` (`Shell_NotifyIconW(NIM_DELETE)`) runs cleanly before spawning new process, resolving multiple taskbar tray icon instances and ghost icons (44/44 passing tests).
* **2026-07-30**: Architect synchronized `spec.md` behavioral contracts with commit `5c2e63a`, adding single-instance process execution scenarios, updating `run_tray` return signature to `TrayExitReason`, and adding `TrayExitReason` and `SingleInstanceGuard` data models.
* **2026-07-30**: Builder updated `architecture.md` Sections 2 and 5 per Architecture Recommendations Report, documenting single-instance process mutex guard, `SingleInstanceGuard` RAII handle, `main` module boundary, and clean tray process restart handoff via `TrayExitReason` enum.
* **2026-07-30**: Builder bumped version to `0.1.9`, updated `CHANGELOG.md`, compiled release binary `syncdir.exe`, pushed `v0.1.9` git tag, verified `cargo publish --dry-run` (26 files, 283.7KiB), and published GitHub Release `v0.1.9` at `https://github.com/wends155/syncdir/releases/tag/v0.1.9` (44/44 passing tests).
* **2026-07-31**: Builder fixed startup timing races in `src/tray.rs`, `src/main.rs`, and `src/sync.rs`. Extended `run_tray` with `initial_dest_online: Vec<bool>` parameter to eliminate startup `DestinationOffline` tray flicker, and refactored `TriggerFullScan` handler to read `source_online_atomic` to resolve source-offline startup scan race (44/44 passing tests).
* **2026-08-01**: Builder entered root `info_span!("syncdir", host = %sys_info.hostname)` in `src/main.rs` to propagate machine identifier across all log lines, and removed duplicate `host` field from `System environment` startup event (44/44 passing tests).
* **2026-08-01**: Builder executed Phase 1 Testing Infrastructure Uplift: added `pretty_assertions = "1"` and `insta = "1"` dev-dependencies, updated all test modules (`src/config.rs`, `src/db.rs`, `src/startup.rs`, `src/sync.rs`, `tests/integration_tests.rs`) with `pretty_assertions::assert_eq` import shadows, and created `tests/snapshot_tests.rs` with 10 snapshot tests covering `Config` debug formatting, validation errors, `SyncError` display output, and `FileRecord` debug structures (54/54 passing tests).
* **2026-08-01**: Builder executed Phase 2 Testing Infrastructure Uplift: added `proptest = "1"` dev-dependency and created `tests/property_tests.rs` with 6 property-based test suites covering block boundary division, Config TOML round-trip serialization, SMB timestamp tolerance, path traversal safety, sync engine idempotency, and delta sync single-block isolation (60/60 passing tests).
* **2026-08-01**: Builder executed Phase 3 Testing Infrastructure Uplift: extracted `TrayState` struct from `run_tray` in `src/tray.rs` encapsulating pure status calculation and tooltip text generation, refactored `run_tray` event loop to delegate to `TrayState`, and added 8 unit tests in `src/tray.rs` covering all `EngineStatus` transitions and tooltip string formatting (68/68 passing tests).
* **2026-08-01**: Architect completed post-implementation audit for the 3-Phase Testing Infrastructure Uplift, confirming 100% plan fidelity, zero test regressions (68/68 passing tests), zero clippy warnings, and clean formatting.
* **2026-08-01**: Architect executed `/update-doc` workflow: synchronized `spec.md` behavioral contracts and verification hash (`204e4b9`), added `TrayState` Public API and Data Models, added `[HAPPY] Pure TrayState status calculation` behavioral scenario, and documented testing frameworks in `spec.md` Integration Points.
* **2026-08-01**: Architect conducted qualitative code review (`/review error handling hardening`), identifying 6 findings across `.expect()` panic risks in `get_archive_path`, pre-1970 timestamp underflow, missing SMB error classification, and silent process spawn failures.
* **2026-08-01**: Builder executed Error Handling Hardening implementation plan: added `SyncError::is_network_offline()` method in `src/error.rs`, refactored `get_archive_path` to return `Result<PathBuf, SyncError>`, added `safe_modified_millis` and `safe_epoch_duration_millis` timestamp safety helpers in `src/sync.rs`, reclassified `SystemTimeError` to `SyncError::Io`, handled `Command::spawn()` failure in `src/main.rs`, and added 5 unit tests (73/73 passing tests).
* **2026-08-01**: Architect completed post-implementation audit (`/audit`), confirming 100% plan fidelity, zero test regressions (73/73 passing tests), zero clippy warnings, and clean formatting.
* **2026-08-01**: Architect completed `/grill-me` alignment and Builder executed implementation plan for `.agents/scripts/check-quality.ps1`: created a PowerShell quality gate automation script executing cargo fmt, cargo clippy, cargo test, and sg scan unconditionally with structured Markdown summary table rendering and soft ast-grep fallback.
* **2026-08-01**: Builder executed implementation plan for review finding remediation: deduplicated `dest_dir` resolution in `get_archive_path` (`src/sync.rs`), expanded `is_network_offline()` in `src/error.rs` to include Win32 codes 65 (`ERROR_NETWORK_ACCESS_DENIED`) and 121 (`ERROR_SEM_TIMEOUT`), added `# Errors` doc comments, and added 2 unit tests (75/75 passing tests).
* **2026-08-01**: Architect completed post-implementation audit (`/audit`), confirming 100% plan fidelity, zero test regressions (75/75 passing tests), zero clippy warnings, and clean formatting.
* **2026-08-02**: Builder executed implementation plan for release profile optimizations: added `[profile.release]` section to `Cargo.toml` with `lto = true`, `codegen-units = 1`, `strip = "symbols"`, and `panic = "abort"`.
* **2026-08-02**: Architect completed post-implementation audit (`/audit`), confirming 100% plan fidelity, zero test regressions (75/75 passing tests), zero clippy warnings, and clean release profile compilation (`cargo build --release` in 1m 06s).
* **2026-08-02**: Builder executed implementation plan for portable static release build script: created `.cargo/config.toml` configuring `+crt-static` for MSVC target, created `.agents/scripts/build-release.ps1` distribution pipeline with mandatory quality gate check, hard `dumpbin` static CRT verification, versioned ZIP archive (`syncdir-v0.1.10-x86_64-windows.zip`), SHA256 checksum generation, and updated `.gitignore` with `dist/` exclusion.
* **2026-08-02**: Builder executed implementation plan for authentication error classification and diagnostic root-cause logging: added `ERROR_LOGON_FAILURE (1326)` to `SyncError::is_network_offline()` in `src/error.rs`, added `target` and `os_error` structured logging fields to all sync failure WARN logs (`run_full_scan` and `start_sync_worker` in `src/sync.rs`), replaced silent `dest.exists()` checks with `std::fs::metadata()` in `src/main.rs` (startup reachability) and `src/sync.rs` (periodic heartbeat), and added transition recovery logging ("Target destination is back online"). Verified with 76/76 passing tests (unit, integration, property, snapshot, doc-tests) and clean format/clippy audits.
* **2026-08-02**: Builder executed implementation plan for Reload Config exit-without-restart fix: wrapped `exit_reason` in `std::rc::Rc<std::cell::Cell<TrayExitReason>>` in `src/tray.rs`, allowing the `move` closure and `run_tray` return statement to share the same mutable cell state. Removed `#[allow(unused_assignments)]` suppression attribute that masked the compiler warning. Verified with 76/76 passing tests (unit, integration, property, snapshot, doc-tests) and clean format/clippy audits.
* **2026-08-02**: Builder executed implementation plan for ast-grep lint suppression audit rule: created `.ast-grep/rules/lint-suppression-audit.yml` to flag all four Rust lint suppression forms (`#[allow]`, `#[expect]`, `#![allow]`, `#![expect]`) as audit hints during `sg scan`. Removed remaining vestigial `#[allow(unused_assignments)]` attribute in `src/tray.rs`. Verified with 76/76 passing tests and zero `sg scan` suppression hits.
* **2026-08-02**: Builder executed implementation plan for review finding remediation and Narsil index registration: added top-level `use std::cell::Cell;` and `use std::rc::Rc;` to `src/tray.rs`, simplified `std::rc::Rc::new(std::cell::Cell::new(...))` to `Rc::new(Cell::new(...))`, and registered `syncdir` (`C:\Users\WSALIGAN\code\syncdir`) in `mcp_config.json` under Narsil `--repos`. Verified with 76/76 passing tests.
* **2026-08-02**: Builder executed implementation plan for full scan network error early-exit: integrated `is_network_offline()` in `run_full_scan` sync loop in `src/sync.rs`. When a target returns a network/auth error (e.g. `ERROR_LOGON_FAILURE (1326)`), the engine logs a single warning ("Target unreachable during full scan, skipping remaining files"), sets `sync_skip_count = total`, and immediately breaks out of the loop, eliminating 160+ repetitive per-file log entries per scan. Verified with 76/76 passing tests (unit, integration, property, snapshot, doc-tests) and clean format/clippy audits.
* **2026-08-02**: Builder executed implementation plan for developer script relocation: moved `build-release.ps1` and `check-quality.ps1` from gitignored `.agents/scripts/` to top-level git-tracked `scripts/` directory, updated path reference in `scripts/build-release.ps1`, untracked `.agents/scripts/build-release.ps1` from Git index, and deleted duplicate copies from `.agents/scripts/`. Verified with `.\scripts\check-quality.ps1` passing all 4 quality gates.
* **2026-08-02**: Architect executed `/update-doc`: synchronized `spec.md` behavioral contracts (verification hash `571f481`, extended network error codes, early-exit full scan scenario, developer scripts integration point) and `README.md` (developer scripts installation usage).
* **2026-08-02**: Builder executed implementation plan for Release v0.1.11: bumped version to `0.1.11` in `Cargo.toml` and `spec.md`, updated `CHANGELOG.md`, generated `release_notes_v0.1.11.md` artifact, and executed `.\scripts\build-release.ps1` producing portable release binary (`dist/syncdir.exe`), distribution archive (`dist/syncdir-v0.1.11-x86_64-windows.zip`), and SHA256 checksum file with zero dynamic CRT dependencies.
* **2026-08-18**: Builder and Architect completed the integration of Knowledge-RAG MCP across TARS workflows (`toolcheck.md`, `build.md`, `plan-making.md`, `feature.md`, `issue.md`) and rules (`coding-standard.md`). Knowledge-RAG is verified during `/toolcheck` and queried first for dependency API/pattern lookups during `/build`, `/plan-making`, `/feature`, and `/issue` before falling back to Context7 or web search. Verified 8/8 grep checks and zero-exit `cargo check`. (Commit `fac230b` on `feature/knowledge-rag-setup`).
* **2026-08-19**: Architect executed `/update-doc`: synchronized `spec.md` behavioral specification verification hash to `fac230b` (0 commits drift), verified alignment across `Cargo.toml [package.description]`, `src/lib.rs //!` overview, and `README.md` first paragraph, and confirmed 0 rustdoc warnings. (Commit `a3c880e` on `feature/knowledge-rag-setup`).
* **2026-08-19**: Builder executed implementation plan for `architecture.md` doc drift remediation following `/architecture` audit: updated Section 6 Dependency Direction Table (adding `startup` to `tray` May Import list), enriched Section 5 `config` module boundary with Win32 path resolution helpers (`resolve_mapped_drive_unc`, `try_resolve_unc_path`, `establish_smb_connection`, `normalize_paths`), and updated Section 12 Dependencies to document the Developer & Agent Tooling ecosystem including Knowledge-RAG MCP (`search_knowledge`). Verified with 85/85 passing tests and 100% clean quality checks.
* **2026-08-19**: Builder executed implementation plan for `/review` findings remediation in `src/config.rs`: added `is_ascii_alphabetic()` guard to `normalize_path` trailing-backslash trim loop (`is_root_drive`), and added an explanatory comment above the `#[allow(non_snake_case)]` Win32 `NETRESOURCEW` FFI struct attribute. Verified with 85/85 passing tests, clean clippy, and zero AST lint regressions.
* **2026-08-20**: Builder executed implementation plan for Knowledge-RAG query-first protocol integration across TARS workflows (`plan-making.md`, `audit.md`) and rules (`builder-rules.md`, `execute-plan-step SKILL.md`, `knowledge-rag-query SKILL.md`). Wired `knowledge-rag-query` skill into `plan-making.md` prerequisites, added Knowledge-RAG guidance to `execute-plan-step` CODE phase, added Knowledge-RAG pattern audit to `audit.md` §2f, added §7.1 Knowledge-RAG Query-First Protocol to `builder-rules.md`, and expanded `knowledge-rag-query` skill scope to cover Architect use cases. Verified with 96/96 passing tests and 100% clean quality gate checks.
* **2026-09-08**: Executed comprehensive Tier-L architectural refactoring across the codebase resolving all findings from the 5-lens code review (`review_report.md`):
  1. Extracted Win32 FFI networking logic (`WNetAddConnection2W`, `WNetGetConnectionW`, UNC / drive resolution) out of `src/config.rs` into a dedicated high-cohesion `src/net.rs` module.
  2. Preserved causal error chains on `SyncError::Db` and `SyncError::Watcher` using `#[source] Option<Box<dyn std::error::Error + Send + Sync>>`, implemented `From` conversions for `rusqlite` and `notify` errors, and extracted standalone `is_network_offline_io` matching raw OS codes.
  3. Fully encapsulated `Config` with private fields, implemented `ConfigBuilder`, provided slice/path getters, introduced `TargetSyncConfig`, and refactored all callers atomically across `src/` and `tests/`.
  4. Optimized delta sync in `src/sync.rs` by introducing `DirtyBlockRange` coalescing contiguous block writes up to 16MB per batch, adding post-loop flush, performing post-sync write verification read-back, and filtering out directory paths to prevent infinite retry loops.
  5. Decoupled directory monitoring in `src/monitor.rs` to log `tracing::error!` and terminate early on channel disconnect.
  6. Decoupled system tray UI in `src/tray.rs` via `TrayActionHandler` trait, enabling generic handler injection into `run_tray<H: TrayActionHandler + ?Sized>`.
  7. Isolated Windows Registry testing in `src/startup.rs` by marking `test_startup_registration_toggle` with `#[ignore]` to protect host registry keys during automated test runs.
  8. Consolidated 3 disjoint mutexes in `MockHashStore` (`src/db.rs`) into a single `Arc<RwLock<MockStoreInner>>` preventing deadlock hazards.
  9. Refactored monolithic `try_main` in `src/main.rs` into a declarative pipeline powered by `SyncDaemon` orchestrator and `DaemonTrayHandler`, adding `test_sync_daemon_lifecycle` and `test_daemon_tray_handler` unit tests.
  Verified with 115 passing tests across unit, integration, property, snapshot, and doctest suites, 0 compiler warnings (`-D warnings`), and clean formatting.

* **2026-08-02**: Implemented `EngineStatus::Degraded` in `src/tray.rs` for partial destination failures. When some (but not all) targets are offline, `overall_status()` returns `Degraded`, painting an orange tray icon border `(255, 140, 0)` while active synchronization continues for online targets (77/77 passing tests). Relocated release scripts to top-level `scripts/` directory, added `ast-grep` suppression audit rule `.ast-grep/rules/lint-suppression-audit.yml`, and published GitHub Release `v0.1.11`.
* **2026-08-02**: Added Windows Mapped Drive support (`X:\...`) and Path Compatibility Filtering (`normalize_path`) in `src/config.rs`. Converts forward slashes `/` -> `\`, trims quotes/whitespace/trailing slashes, repairs single-backslash UNC prefixes (`\172...` -> `\\172...`), and explicitly validates/logs Windows mapped drive target paths (`X:\...`). Published GitHub Release `v0.1.12` (`https://github.com/wends155/syncdir/releases/tag/v0.1.12`).
* **2026-08-02**: Implemented startup destination reachability pre-checks and drive letter root normalization. Updated `run_full_scan()` in `src/sync.rs` to verify `dest_dir` reachability upfront (`dest.exists() && dest.is_dir()`) before scanning source files, returning `Ok(false)` cleanly when offline to eliminate 50+ duplicate file skip log entries per scan. Updated `normalize_path()` in `src/config.rs` to automatically convert 2-character drive letter roots (`R:`) to root path format (`R:\`). Added Win32 error codes `3` (`ERROR_PATH_NOT_FOUND`) and `15` (`ERROR_INVALID_DRIVE`) to `SyncError::is_network_offline()` in `src/error.rs`. Verified with 83/83 passing tests (unit, integration, property, snapshot, doc-tests) and 100% clean quality gate results.
* **2026-08-02**: Implemented Win32 Mapped Drive UNC Resolution and Reachability Fallback. Added `resolve_mapped_drive_unc(drive_letter)` utilizing Win32 `WNetGetConnectionW` FFI (`mpr.dll`) in `src/config.rs` to resolve local drive letters (`R:`) to their underlying remote UNC network paths (`\\172.16.0.193\share`). Added `try_resolve_unc_path(path)` helper. Integrated reachability fallbacks into `src/main.rs` (initial startup check) and `src/sync.rs` (`run_full_scan`), automatically detecting and using resolved UNC network paths when mapped drives report `os error 3` due to UAC session token isolation or mount subfolder redundancy, while logging diagnostic telemetry. Verified with 84/84 passing tests and zero clippy/fmt errors.
* **2026-08-02**: Implemented Config Path Resilience, Format Validation, Automatic Struct Path Normalization, and Comprehensive Testing. Added `normalize_paths(&mut self)` in `src/config.rs` to automatically normalize `source_dir`, `dest_dir`, and `dest_dirs` upon `Config::load()` and `Config::test_default()`. Enhanced `Config::validate()` to enforce format validation on `source_dir` (must start with drive letter `C:\...`, mapped drive `R:\...`, UNC network share `\\...`, or Unix absolute path `/...`), returning `SyncError::Validation` if a relative path is passed. Added `resolved_source_dir(&self) -> PathBuf` leveraging `try_resolve_alternate_path` for SMB connection & mapped drive fallback, and integrated into `main.rs`, `sync.rs`, and `monitor.rs`. Enhanced `resolved_dest_dirs()` with case-insensitive path comparison on Windows (`Z:\Backup` vs `z:\backup`). Added unit tests in `src/config.rs`, snapshot test in `tests/snapshot_tests.rs`, and property test in `tests/property_tests.rs`. Verified with 93/93 passing tests and 100% clean quality gate checks.
* **2026-08-02**: Executed full codebase compliance audit via `/audit`. Verified 100% compliance across formatting (`cargo fmt`), linter (`cargo clippy`), 94 automated unit/property/snapshot/doctest suites, and `ast-grep` security rules. Updated [`README.md`](file:///c:/Users/WSALIGAN/code/syncdir/README.md#L70-L86) with an explicit Windows path formatting summary table. Added `test_preprocess_dest_dirs_multiline_array()`, `test_preprocess_dest_dirs_mixed_quotes_and_commas()`, and `test_config_load_invalid_missing_comma_in_dest_dirs()` in `src/config.rs` testing multi-line array parsing, trailing commas, mixed quotes, and asserting syntax error returns on missing commas. Published GitHub Release `v0.1.13` (`https://github.com/wends155/syncdir/releases/tag/v0.1.13`).
* **2026-09-08**: Remediated all 34 qualitative findings from the 5-lens code review (`review_report.md`) across logic, performance, security, design, and API dimensions. Implemented single-pass streaming read/write loop and `DirtyBlockRange` seek batching in `src/sync.rs`, eliminating dual-pass reads on >10MB files. Added destination length boundary check (`dest_len`) preventing zero-fill corruption on truncated target files. Implemented small file (<10MB) hash-skipping fast path using direct `fs::copy`. Plumbed alternate path sync in `run_full_scan`. Implemented recursive child file sync on directory renames. Bounded worker command queues at 50,000 entries and guarded deletion executions with `source_online && dest_online`. Added source and destination offline retry rescheduling via `retry_interval_seconds` and catch-up scans on reconnect. Hardened `is_safe_relative_path` with DOS device names (`CONIN$`, `CONOUT$`, `CLOCK$`) and guarded `sync_file` against symlinks via `fs::symlink_metadata`. Modernized `HashStore` trait to accept `&Path` and return `PathBuf`, added deterministic `path_to_sqlite_key` normalization, consolidated `MockHashStore` locks, and decoupled `SqliteHashStore::new` from `Config` via `StoreConfig` using prepared statement caching (`prepare_cached`) and transactions. Extracted `SyncDaemon` and `DaemonTrayHandler` into `src/daemon.rs` and re-exported via `src/lib.rs`, slimming `src/main.rs`. Centralized system root resolution in `src/config.rs` with resilient explorer lookup in `src/tray.rs`. Fixed Win32 error code 3 classification in `src/error.rs` and preserved causal chains in `SyncError::Config`. Added 5 new regression tests across `src/sync.rs`, `src/daemon.rs`, and `tests/integration_tests.rs`. Verified zero-exit status across all 128 tests, zero clippy warnings, and clean formatting.
* **2026-09-08**: Completed `/update-doc` workflow following the Tier-L refactor. Synchronized `spec.md` behavioral contracts against commit `8e6f4a7` and updated package version to `0.1.13`. Added contracts for all 9 modules (`config`, `db`, `sync`, `daemon`, `net`, `startup`, `monitor`, `tray`, `error`) including `DirtyBlockRange` batching, `StoreConfig`, `SyncDaemon`, and `DaemonTrayHandler`. Enriched crate-level rustdoc in `src/lib.rs` with architectural guide and doctest example. Added comprehensive doc comments to `HashStore` methods in `src/db.rs`, `DirtyBlockRange` methods in `src/sync.rs`, and `DaemonTrayHandler`/`SyncDaemon` in `src/daemon.rs`. Updated `README.md` API surface and feature list. Verified zero-exit status on all linters, formatters, and tests (128 unit/integration/property/snapshot tests passing + 2 doctests).
* **2026-09-08**: Remediated `SyncError` asymmetric design and context-erasing stringification patterns across `syncdir` with regression prevention. Upgraded `LockPoison`, `Tray`, and `Registry` variants to 2-tuple `(String, #[source] Option<Box<dyn std::error::Error + Send + Sync>>)` with `#[non_exhaustive]` on `SyncError`. Generalized `LockPoison` error display to `"Lock poisoned: {0}"` for module-agnostic mutex errors. Added paired constructors (`lock_poison_with_source`, `tray_with_source`, `registry_with_source`) with full doc comments. Replaced 25+ `.map_err` stringification sites across `src/tray.rs`, `src/startup.rs`, `src/db.rs`, `src/daemon.rs`, and `src/main.rs` with `*_with_source` constructors. Discriminated `std::io::ErrorKind::NotFound` from unexpected errors in `src/startup.rs` (`is_registered`, `unregister`). Replaced silent error swallowing (`let _ =`) with proper error propagation in `DaemonTrayHandler::on_sync_now` and structured `tracing` logging in `src/tray.rs` (`open_path`) and `src/net.rs` (`establish_smb_connection`). Optimized `get_block_hashes` in `src/db.rs` to zero-allocation blob reads via `val_ref.as_blob()`. Optimized `is_safe_relative_path` in `src/sync.rs` to zero-allocation case comparison via `eq_ignore_ascii_case`. Added ast-grep prevention rule `.ast-grep/rules/error-stringification.yml` targeting stringification inside `.map_err` closures. Updated `tests/snapshot_tests.rs` with `test_sync_error_display_lock_poison` snapshot. Synchronized `architecture.md § 8` and `spec.md § Data Models`. Zero-exit status across all 133 tests, clippy, fmt, and ast-grep rules.
* **2026-09-08**: Implemented strongly-typed path domain modeling (`TargetDir` and `DestinationCollection`) in `src/config.rs`. Refactored `Config` to store `source_dir: TargetDir` and `destinations: DestinationCollection`, eliminating split-brain `dest_dir` / `dest_dirs` storage. Added `RawConfig` Serde bridge with `#[serde(from = "RawConfig", into = "RawConfig")]` guaranteeing 100% roundtrip fidelity across legacy and multi-destination TOML schemas without breaking external callers (`main.rs`, `daemon.rs`, `monitor.rs`). Maintained infallible `ConfigBuilder::build() -> Config`, deferring format/path validation to `Config::validate()`. Updated debug layout snapshots in `tests/snapshots/` for `test_config_snapshot_basic` and `test_config_snapshot_multi_dest`. Maintained 100% test passing rate across all 138 unit, integration, property, and snapshot tests with zero clippy warnings and clean formatting.
* **2026-09-08**: Remediated all 31 findings from the 5-lens code review (`review_report.md`) across 9 phases (44 steps, 9 checkpoint commits) and verified via `/audit`. Addressed Unicode UNC slicing panics, empty watcher path loops, ISP decoupling, path containment validation, TOML escaping, extended Win32 offline codes, database key normalization, `HashStore` trait abstraction (eliminating surrogate `file_id`), bulk N+1 preloading via `list_all_records`, block hash UPSERTs, cascading directory deletions, `verify_writes` small-file enforcement, TOCTOU length protection, reparse/junction write shields, `sync_file_to_dest_buffered` decomposition, queue eviction, dynamic debounce sleeping, `symlink_metadata` syscall elimination, scratch buffer reuse, archive capping, domain typing (`TargetDir`, `ConnectivityState`, `WatcherState`), DIP `SyncEngineFactory` abstraction, non-blocking UI startup, atomic tray reload protection, and `architecture.md` synchronization. Verified with 164 passing tests (0 failures, 1 ignored), clean clippy (`-D warnings`), clean formatting, and zero AST grep security findings. Verdict: ✅ Pass.

## 3. Context Compression
* **Feature:** Remediate all 31 findings from the 5-lens code review (`review_report.md`) across logic, design, performance, security, and API dimensions in syncdir v0.1.14.
* **Changes:** Hardened `find_mapped_drive_for_unc` with character-boundary slicing and `GetLogicalDrives` bitmask. Added empty path guard in `DirectoryWatcher::start` and decoupled watcher from `Config` (ISP). Added recursive path containment validation, 64MB upper bound on block size, and threshold invariant enforcement in `Config::validate`. Preserved valid TOML escapes in `preprocess_config_toml`. Added Win32 offline codes 1222, 1231, 1232 to `is_network_offline_io`. Guarded `path_to_sqlite_key` against root slash paths. Migrated `HashStore::get_block_hashes` from surrogate `file_id` to `&Path`. Added `HashStore::list_all_records` to eliminate N+1 queries during full scan. Switched block hash updates to UPSERT with trailing prune in `SqliteHashStore::save_file`. Implemented cascade deletions on directory removal. Enforced `verify_writes` on small-file sync. Eliminated TOCTOU size truncation using actual bytes streamed. Shielded sync from Windows junctions and symlinks via reparse tag `0x400` checks. Decomposed `sync_file_to_dest_buffered` into focused helpers. Evicted permanent validation errors from worker queue and implemented dynamic deadline computation. Replaced redundant `symlink_metadata` syscalls in directory scan. Reused scratch buffers across large file syncs. Added retention cap to `.syncdir_archive`. Standardized `TargetDir` in `TargetSyncConfig` and added zero-copy `destinations()` slice. Introduced `SyncEngineFactory` trait for DIP. Deferred network probes to background workers for non-blocking UI startup. Protected tray config reload with atomic guard. Replaced boolean blindness with `ConnectivityState` and `WatcherState` enums in `TrayState`. Updated `architecture.md` to reflect `daemon`, `net`, `path_util`, and unidirectional module flow.
* **New Constraints:** 
  - Raw `.unwrap()` / `panic!()` / `todo!()` are banned in production code (triggers `unwrap-in-production`).
  - Dynamic string formatting inside database executions is banned (triggers `sql-injection`).
  - Raw filesystem functions (including deletions) must be wrapped, config-driven, or validated (triggers `path-traversal-leak` hint).
  - Environment variables must be central in `src/config.rs` (triggers `scattered-env-var`).
  - `startup` module may import `config` and `error`, but must NOT import `sync`, `db`, `monitor`, `tray`.
  - `MockHashStore` and `MockStartupRegistry` are available for isolated in-memory unit tests.
  - Use standard UTF-8 comparison characters (`≥`) in Markdown documentation instead of LaTeX math syntax (`$\ge$`).
  - `dest_dir` is optional in `Config`; configurations can specify `dest_dir`, `dest_dirs`, or both. `resolved_dest_dirs()` requires at least one valid destination path.
  - Rust 2024 edition requires `unsafe extern "system"` modifier syntax for Win32 FFI blocks.
  - `TriggerFullScan` evaluates `source_online_atomic` to prevent startup scan race conditions.
  - "Reload Config" validates configuration and returns `TrayExitReason::Restart` to exit winit event loop cleanly before process respawn.
  - `SingleInstanceGuard` enforces single-instance process execution using Win32 named mutex `Local\syncdir_single_instance`.
  - `run_tray` receives `initial_dest_online: Vec<bool>` from `main.rs` reachability checks to populate context menu items and avoid initial `DestinationOffline` status flicker.
  - `TrayState::overall_status()` distinguishes `Degraded` (some destinations offline, orange icon) from `DestinationOffline` (all destinations offline, yellow icon).
  - `normalize_path` sanitizes all source and destination paths for cross-platform forward-slash conversion, trailing slash trimming, UNC prefix repair, and drive letter root trailing backslash enforcement (`R:` -> `R:\`).
  - `try_resolve_unc_path` queries Win32 `WNetGetConnectionW` to automatically translate mapped drive letters (`R:`) into full UNC network share paths (`\\172.16.0.193\share`) when drive letter reachability checks fail.
  - `establish_smb_connection` queries Win32 `WNetAddConnection2W` to automatically establish SMB network authentication for UNC paths using cached Windows Credential Manager entries when logon errors (`os error 1326`) occur.
  - `try_resolve_alternate_path` provides bidirectional path resolution (UNC ⟷ Mapped Drive) and SMB session initialization for startup and full scan reachability verification.
  - `run_full_scan` performs an upfront destination presence check (`dest.exists() && dest.is_dir()`) before source directory scanning to avoid duplicate file skip warnings when targets are unmounted or unreachable.
  - `tracing_subscriber` MUST use `.with_ansi(false)` on file appender and stdout layers to prevent raw ANSI escape byte pollution in log files and command prompt text.
  - `knowledge-rag` MCP tool `search_knowledge` is the query-first convention for dependency API and pattern research across TARS workflows (`/toolcheck`, `/build`, `/plan-making`, `/feature`, `/issue`).
  - `src/net.rs` exclusively owns Win32 networking FFI and path translation; `src/config.rs` is strictly pure configuration parsing.
  - `Config` fields are private; instantiation outside `Config::load` must flow through `ConfigBuilder`.
  - `Config` path storage uses `TargetDir` and `DestinationCollection`; paths are normalized automatically upon construction.
  - `ConfigBuilder::build()` is infallible; format and existence validation is deferred to explicit `Config::validate()` calls.
  - Serialization and deserialization of `Config` must flow through `RawConfig` to maintain symmetric TOML compatibility across both legacy single and modern multi-destination formats.
  - `SyncError` wrapping foreign error types must use `Option<Box<dyn std::error::Error + Send + Sync>>` with `#[source]` to preserve error chains.
  - `SyncError` is `#[non_exhaustive]`; external matches must include wildcard arms.
  - Lossy error stringification inside `.map_err()` closures is prohibited and guarded by ast-grep rule `error-stringification-in-map-err`.
  - Windows Startup Registry operations must discriminate `ErrorKind::NotFound` from permission or access denial errors.
  - Delta file syncing must coalesce writes using `DirtyBlockRange` bounded to 16MB batches and perform a post-loop `flush()`.
  - Delta file syncing must verify `dest_len` against expected offsets to re-write missing blocks and eliminate zero-fill expansion bugs.
  - Files <10MB bypass block hashing and execute directly via `fs::copy`.
  - `DirectoryWatcher` must break and terminate cleanly on channel disconnect without dropping events silently.
  - `run_tray` accepts `Arc<H>` where `H: TrayActionHandler + ?Sized` to decouple UI from daemon lifecycle.
  - `SyncDaemon` manages background worker and watcher threads, coordinating clean atomic shutdown.
  - Host Windows Registry mutation tests must be ignored (`#[ignore]`) during CI/automated test runs.
  - Module dependency direction is strictly acyclic: `net` and `config` communicate via independent data flow without circular imports.
  - `DirectoryWatcher::start` must only accept `source_dir: impl AsRef<Path>`, not full `Config` (ISP compliance).
  - `HashStore::get_block_hashes` must accept `&Path`, never SQLite surrogate integer IDs.
  - `SyncEngineFactory` trait must be used for daemon engine and store instantiation (DIP compliance).
  - Tray watcher status must be communicated via domain enums `ConnectivityState` and `WatcherState`, never raw bools.
  - File length changes during streaming must use actual bytes read (`total_bytes_read`) to avoid TOCTOU truncation.
  - Directory removal must cascade deletion to SQLite child records (`delete_file_records_under_prefix`).
  - Worker debounce loop must compute dynamic sleep deadlines rather than busy-polling O(N) retain.
* **Pruned:** Manual checks for unwraps, env variables, filesystem operations, release packaging, mock testing structures, LaTeX rendering defects, single-destination configuration constraints, startup scan timing race conditions, startup tray status flicker, process restart ghost tray icons, duplicate process launches, architecture module boundary status drift, all-or-nothing destination offline status misrepresentation, un-normalized mapped drive path handling, SMB logon credential authentication failures, duplicate offline scan warning loops, raw ANSI log escape character corruption, unclarified README Windows path formatting gotchas, Knowledge-RAG workflow integration checks, uncoalesced block seeks over network shares, host registry test pollution, monolithic main loop coupling, unpreserved error causes, dual-pass I/O inefficiencies, truncated destination zero-fill corruption, directory rename recursion gaps, unbounded worker queues, asymmetric SyncError design, lossy error stringification, split-brain configuration path management, Unicode UNC slice panics, empty watcher path loops, circular config-net dependencies, surrogate database key leakage, N+1 full scan database queries, full block hash DELETE+INSERT churn, orphaned child SQLite records, TOCTOU length truncation, Windows junction/symlink write traversal, and UI startup blocking on offline shares are now resolved and automated.

---

> 📝 **Context Update (2026-09-09):**
> * **Feature:** 45-Finding Comprehensive Review Remediation, Architecture Audit, and Documentation Synchronization
> * **Changes:**
>   - Remediated all 45 findings across logic, security, performance, design, API, and testing according to approved implementation plan.
>   - Introduced `SyncError::WriteVerificationFailed` and exponential backoff retry loop in `start_sync_worker` (Findings 1, 31).
>   - Plumbed active resolved UNC paths to worker sync and delete operations (Finding 2).
>   - Fixed SQL LIKE wildcard injection via exact prefix comparison `substr(relative_path, 1, length(?1) + 1) = ?1 || '/'` (Finding 3).
>   - Decoupled `prune_archive` from deletion hot-path to periodic/post-scan maintenance; added depth cap (32) and symlink/junction skipping (Findings 4, 8).
>   - Intermediate ancestor directory junction verification in `verify_destination_not_reparse` (Finding 9).
>   - 64KB streamed small-file verification; worker scratch buffer reuse in delta sync (Findings 7, 21).
>   - Resilient directory scanning in `scan_dir`; case-insensitive deletion tracking in `run_full_scan` (Findings 16, 17).
>   - Decomposed `start_sync_worker` into `DebounceQueue`, `ReachabilityMonitor`, `SyncWorkerState` (Finding 11).
>   - Extracted `TrayController` from `run_tray`; qualified `%SystemRoot%\explorer.exe` in `open_path` (Findings 12, 41).
>   - Defined `trait NetworkResolver` (`Win32NetworkResolver`, `MockNetworkResolver`) and implemented `MockSyncEngine` (Findings 13, 14).
>   - Encapsulated `TargetSyncConfig` with `TargetSyncConfigBuilder` and `TargetDir::new` (Findings 19, 27).
>   - Replaced stdlib arithmetic property tests with domain invariant evaluations; updated snapshot goldens (Findings 44, 45).
>   - Fully synchronized `architecture.md` (all 16 sections, layout, boundaries, mocks) and `spec.md` (`> Last verified against: 8633ddd`).
> * **New Constraints:**
>   - All write verification failures must use `SyncError::WriteVerificationFailed` and be retried via exponential backoff; permanent eviction is reserved for `SyncError::Validation`.
>   - Database directory prefix queries must use `substr(relative_path, 1, length(?1) + 1) = ?1 || '/'` to avoid LIKE wildcard expansion.
>   - `verify_destination_not_reparse` must traverse all intermediate ancestor directories between destination root and target path.
>   - `prune_archive` must strictly enforce `depth <= 32` and skip symlinks/junctions.
>   - External process spawning for system files (`explorer.exe`) must use fully verified absolute paths (`%SystemRoot%\explorer.exe`).
>   - `TargetSyncConfig` fields are private; callers must use getters or `TargetSyncConfigBuilder`.
>   - All four inter-module boundaries (`HashStore`, `RegistryBackend`, `NetworkResolver`, `SyncEngine`) must maintain in-memory mock implementations.
---

> 📝 **Context Update (2026-09-09):**
> * **Feature:** Synchronization Algorithms & Corruption Prevention Hardening (31 Review Findings Remediation)
> * **Changes:**
>   - Remediated all 31 review findings across 6 component groups (`src/error.rs`, `src/db.rs`, `src/sync.rs`, tests).
>   - Enriched `SyncError::WriteVerificationFailed` with `block_index: Option<u64>`, `expected_hash`, and `actual_hash` hex diagnostics (O10).
>   - Tightened database encapsulation by reducing `path_to_sqlite_key` and `FileRecord.id` to `pub(crate)`, adding `is_tracked()`, and replacing unindexed table scan queries in `SqliteHashStore::delete_file` with sargable range scan (`relative_path >= ?1 AND relative_path < ?2`) (O5, O14, O16).
>   - Added `HashStore::save_files_batch` with single-transaction atomic batching (O4).
>   - Switched reparse point and symlink auditing to `fs::symlink_metadata` across all traversal paths and intermediate directory components (`is_reparse_or_symlink`, `verify_source_not_reparse`, `verify_destination_not_reparse`, `delete_file_from_dest`), preventing symlink escape and junction traversal (O2, O9).
>   - Hardened `is_safe_relative_path` to reject trailing spaces/dots and DOS device names before stem truncation (O8).
>   - Added reparse verification directory cache (`verify_destination_not_reparse_cached`) to eliminate redundant SMB RPC round-trips during full scans (O13).
>   - Pinned `block_size` in `DirtyBlockRange::new(block_size)` constructor, removing method parameter drift (O6).
>   - Replaced 4-consecutive `i64` transposition hazard in `is_metadata_up_to_date_raw` with structured `FileMetadataSnapshot` (O17).
>   - Removed dangerous default implementations discarding `_dest_dir` from `SyncEngine` trait (O7).
>   - Added traversal completeness tracking in `scan_dir` (`scan_complete: &mut bool`); skipped deletions in `run_full_scan` if any directory encountered `PermissionDenied` or max depth (O3).
>   - Normalized full scan relative paths to forward slashes before SQLite key and source lookup comparisons, permanently preventing false deletions on Windows (O1).
>   - Enriched `ScanOutcome::PartialFailure` with `delete_failed: usize`, and routed general I/O failures through exponential backoff retry in worker loop (O11, O12, O15).
>   - Returned raw Win32 OS error 53 (ERROR_BAD_NETPATH) on missing destination in `delete_file_from_dest` to preserve network offline detection (O18).
>   - Equipped `LocalSyncEngine` with `resolved_dest: Option<PathBuf>` and `with_resolved_dest`, eliminating static DIP bypass in `run_full_scan`.
>   - Expanded automated test suite to 208 passing tests (+11 net-new tests: 10 unit + 1 snapshot), with zero test regressions and 100% clean formatting and linting.
> * **New Constraints:**
>   - Directory scanning must always track completeness via `scan_complete: &mut bool`; deletion propagation MUST be skipped if the scan was incomplete.
>   - Relative paths for SQLite keys and source lookups must be normalized to forward slashes before comparison.
>   - Reparse and junction checks must ALWAYS use `fs::symlink_metadata`, never `entry.metadata()`.
>   - `delete_file` in `HashStore` must use sargable index range queries (`>= ?1 AND < ?2`) rather than string manipulation functions in SQL.
>   - `DirtyBlockRange` must be constructed with its fixed block size; callers cannot supply varying block sizes to `add_block` or `flush`.
>   - General I/O errors in the worker loop must use `calculate_exponential_backoff` and cap at 10 retry attempts before eviction.
> * **Pruned:**
>   - Obsolete review findings discussions, pre-normalization path separator mismatch bugs, unindexed SQL substring scan overhead, parameter transposition hazards, and unhandled permission denied subtree deletion hazards are resolved and closed.

---

> 📝 **Context Update (2026-09-09):**
> * **Feature:** Deferred Findings Remediation (F1–F15) & `src/sync/` Submodule Decomposition
> * **Changes:**
>   - Remediated all 15 deferred review findings (F1–F15) across synchronization algorithms, concurrency, performance, security, and architectural cohesion.
>   - Encapsulated `StoreConfig` and `DirtyBlockRange` with private fields, validated constructor invariants, and read-only accessors.
>   - Added cooperative cancellation token (`SyncEngine::run_cancellable_full_scan`) accepting `&AtomicBool` to interrupt long-running directory scans gracefully during shutdown.
>   - Added $O(1)$ peek `DebounceQueue` min-heap auxiliary index (`BinaryHeap<Reverse<(Instant, PathBuf)>>`) with lazy stale eviction, eliminating $O(N)$ CPU starvation under load.
>   - Implemented worker queue overflow recovery triggering an automated catch-up full scan once the queue drains below threshold.
>   - Added post-stream source metadata re-verification (`post_meta.len()` and `safe_modified_millis`) in delta sync to prevent chimeric stale mtime commits.
>   - Added post-write destination length validation (`dest_file.metadata()?.len() == total_bytes_read`) preventing truncated delta sync corruption.
>   - Bound `bytes_copied` in `sync_small_file` to actual stream byte counts, preventing corrupted metadata recording on truncated copies.
>   - Replaced non-atomic archive destination check-then-act with an incrementing thread-safe nonce (`ARCHIVE_NONCE`) and atomic rename.
>   - Guarded database record deletion in `delete_file_from_dest` with destination reachability checks, preventing cache purge on transient network disconnects.
>   - Introduced tiered write verification (`VerificationMode`: `Disabled`, `MetadataAndFlush`, `Sampled`, `Full`), reducing SMB bandwidth saturation.
>   - Reused `DirtyBlockRange` buffer allocation across files in `SyncWorkerState`, eliminating 16MB allocation churn.
>   - Consolidated small-file SMB operations into a single open handle with streaming Blake3 hashing and inline timestamp alignment.
>   - Integrated `HashStore::save_files_batch` into full scan loops with 500-record batch commits, minimizing SQLite transaction overhead.
>   - Implemented atomic small-file write staging via RAII `TempFileGuard` writing to sibling `.syncdir_tmp` files before atomic rename.
>   - Decomposed monolithic 3,619-line `src/sync.rs` into 9 high-cohesion submodules in `src/sync/` (`mod.rs`, `engine.rs`, `delta.rs`, `small_file.rs`, `archive.rs`, `path_safety.rs`, `scanner.rs`, `worker.rs`, `mock.rs`) while preserving 100% public API compatibility.
>   - Expanded automated test suite to 230 passing tests (184 lib unit, 3 bin unit, 10 integration, 8 property, 20 snapshot, 5 doctests) with zero warnings.
> * **New Constraints:**
>   - Trait methods for `LocalSyncEngine` reside in `src/sync/engine.rs` delegating to specialized inherent methods in submodules.
>   - Directory scanning functions (`scan_dir`, `run_cancellable_full_scan_impl`) are scoped to `pub(crate)` in `src/sync/scanner.rs`.
>   - Worker coordination structures (`DebounceQueue`, `ReachabilityMonitor`, `SyncWorkerState`, `SyncWorkerContext`) are scoped to `pub(crate)` in `src/sync/worker.rs`.
>   - Small-file writes MUST stage to sibling `.syncdir_tmp` files and atomically replace destination via `fs::rename`.
>   - Delta synchronization MUST re-verify source file metadata post-stream and confirm destination file length before committing hashes to SQLite.
>   - Archive file paths MUST incorporate unique thread-safe timestamps and nonces to guarantee atomic rename isolation.
> * **Pruned:**
>   - Monolithic `src/sync.rs` file removed and superseded by modular `src/sync/` hierarchy.
>   - Unindexed linear scan in `DebounceQueue::earliest_deadline()` replaced by min-heap index.
>   - Transient network drop signature purge in `delete_file_from_dest` eliminated.
>   - Single-file unbatched SQLite transactions in `run_full_scan` replaced by batch persistence.

---

> 📝 **Context Update (2026-09-09):**
> * **Feature:** Compile-Time Icon Generation & Tray Subsystem Hardening (Remediating All 13 Review Findings)
> * **Changes:**
>   - Extracted compile-time RGBA icon generation into pure leaf submodule [`src/tray/assets.rs`](file:///c:/Users/WSALIGAN/code/syncdir/src/tray/assets.rs) using `const fn generate_status_rgba`, embedding 5 deterministic 32×32 status bitmaps into static `.rdata` table `STATUS_RGBA: [[u8; 4096]; 5]` with zero runtime heap allocations.
>   - Implemented zero-panic, non-poisoning icon cache provider (`get_cached_icon`) utilizing `OnceLock<[Icon; EngineStatus::COUNT]>` array lookup, eliminating SipHash overhead, 4KB heap re-allocations on query, and unwrap-in-production risks while providing graceful fallback to `Healthy` icon with warning diagnostics.
>   - Embedded Windows application icon resource (`syncdir.ico`, 32×32 32bpp BGRA DIB + 1-bit mask) into the PE executable binary (`RT_GROUP_ICON`) via `build.rs` and `winres` build-dependency.
>   - Added state-transition gating (`last_icon_status: Option<EngineStatus>`) in `TrayController::repaint`, eliminating redundant Win32 `Shell_NotifyIconW(NIM_MODIFY)` ALPC IPC overhead when status is unchanged, and updating status strictly on successful Win32 application.
>   - Encapsulated user-facing display strings within `DestinationState` (`display_label: String` with getter `display_label(&self) -> &str`), and refactored both `TrayController::new` and `handle_status_update` to eliminate domain formatting duplication.
>   - Hardened `scripts/build-release.ps1` by prepending early Windows Kits 10 SDK `rc.exe` discovery before Phase 1 quality gates and writing BOM-less UTF-8 SHA256 checksums in Phase 6.
>   - Updated `architecture.md` Sections 4, 5, 6, and 12 to document `build.rs`, `syncdir.ico`, and `tray::assets` module boundaries and dependencies.
>   - Expanded automated test suite to 238 passing tests (192 lib unit, 3 bin unit, 10 integration, 8 property, 20 snapshot, 5 doctests) with zero warnings, zero clippy lints, and zero AST grep violations.
> * **New Constraints:**
>   - Runtime image decoding/rasterization crates (e.g. `image`, `resvg`, `tiny-skia`) remain prohibited in `[dependencies]`.
>   - Tray icon pixel buffers MUST be pre-evaluated at compile time into `.rdata` static arrays.
>   - Production icon retrieval in `tray::assets` MUST remain zero-panic and never poison the global cache upon transient OS GDI errors.
>   - Win32 tray repaint operations MUST gate `set_icon` on state transitions to minimize system shell ALPC IPC chatter.
> * **Pruned:**
>   - Monolithic procedural pixel rasterization in `src/tray.rs` eliminated.
>   - SipHash-based `OnceLock<HashMap<EngineStatus, Icon>>` replaced by $O(1)$ array indexing.
>   - Duplicated path and UNC label formatting across `TrayController` menu items eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Comprehensive Remediation of 29 Review Findings across Sync Engine, Worker Loop, Test Doubles, Scanner, and Path Safety
> * **Changes:**
>   - Remediated all 29 qualitative architectural findings from `review_report.md` across 26 steps and 6 sequential checkpoints (`480d3d1`, `d8c717e`, `183c70b`, `54b0364`, `a4b9954`, `677795a`).
>   - **Delta Sync Integrity & Recovery**: `LocalSyncEngine::sync_delta_large_file_core` tracks `dirty_blocks_written` and invokes `self.db.delete_file` on in-place write or verification failure, invalidating stale SQLite block signatures. `DirtyBlockRange` resets internal buffers on seek/flush error. `VerificationMode::Full` coalesces contiguous readbacks up to 4MB.
>   - **Non-Fatal Timestamp Updates**: Softened `dest_file.set_times()` failures in `sync_small_file_core` and `sync_delta_large_file_core` to `tracing::warn!` logs to prevent aborting verified byte copies on restrictive SMB shares.
>   - **Reparse & Path Safety Hardening**: `verify_destination_not_reparse` and `verify_destination_not_reparse_cached` validate `dest_dir` root using `is_reparse_or_symlink_meta`. `verified_dirs` cache is bounded to 1,000 entries with FIFO eviction. `archive_dest_file_only` verifies `.syncdir_archive` root against reparse points. `is_safe_relative_path` normalizes Unicode superscripts (`¹`, `²`, `³`) and rejects Win32 wildcard characters (`*`, `?`, `<`, `>`, `|`, `"`).
>   - **Hexagonal Test Doubles & DIP Decoupling**: Added default `is_destination_accessible` method to `NetworkResolver`; enhanced `MockNetworkResolver` with accessible toggles and Win32 error 53 (`ERROR_BAD_NETPATH`). Upgraded `MockHashStore` with `set_error_hook`, batch save counters, and lowercase keys for `COLLATE NOCASE` SQLite parity. Upgraded `MockStartupRegistry` with `set_injected_error`. Upgraded `MockSyncEngine` with `failed_calls` and `prune_archive_calls` tracking. Completely eliminated all 4 ad-hoc test doubles (`StubValidationFailEngine`, `BatchTrackingStore`, `FailingHashStore`, `TruncatingMockStore`).
>   - **Worker Loop Decomposition & Offline Drainage**: Decomposed `start_sync_worker` loop into `SyncWorkerState` helper methods (`mark_needs_catchup_scan`, `clear_needs_catchup_scan`, `should_trigger_catchup_scan`). Offline guard halts queue draining during outages to prevent 1.8M allocations/hr churn. Permanent deletion failure after 10 retries triggers catchup scan on reconnect. Removed extraneous `path.clone()` calls.
>   - **Daemon & Scanner Hygiene**: `DaemonTrayHandler` holds injected `NetworkResolver` and revalidates target loops on config reload. Scanner deletion phase terminates early on `is_network_offline()`. Scanner uses zero-allocation borrowed `&str` lookups (`cached_lookup`). Archive pruning errors are logged. Removed dead `sync_file_buffered` trait method. Enriched `SyncEngine` and `SyncStatusObserver` with `# Examples` and `# Errors` doc comments.
>   - Verified across full test suite: 263 passed (213 unit, 3 main, 12 integration, 8 property, 20 snapshot, 7 doctests), 0 failed, 1 ignored. Clean formatting and clippy (`-D warnings`).
> * **New Constraints:**
>   - Delta synchronization write or verification failures MUST delete the file signature from `HashStore` to force full transfer on next pass.
>   - `DirtyBlockRange` MUST invoke `self.reset()` upon seek or write error before returning.
>   - All reachability checks in worker and daemon MUST query `NetworkResolver::is_destination_accessible` rather than calling `std::fs::metadata` directly on host OS paths.
>   - Ad-hoc test doubles in test modules are prohibited; use canonical mocks (`MockHashStore`, `MockSyncEngine`, `MockNetworkResolver`, `MockStartupRegistry`).
>   - `is_safe_relative_path` MUST reject Win32 wildcards and normalize Unicode superscripts before DOS reserved name evaluation.
> * **Pruned:**
>   - All 4 ad-hoc test doubles (`StubValidationFailEngine`, `BatchTrackingStore`, `FailingHashStore`, `TruncatingMockStore`) deleted.
>   - Dead `sync_file_buffered` method deleted from `SyncEngine`.
>   - Stale SQLite signature retention on in-place delta sync failure eliminated.
>   - Worker offline queue churning (1.8M heap allocations/hr) eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Remediation of 6 Critical Findings from Multi-Lens Qualitative Codebase Review
> * **Changes:**
>   - Remediated all 6 🔴 Critical architectural and correctness defects identified in `review_report.md` via TDD red-green cycles across 16 implementation steps and 7 git checkpoints (`80ecb13`, `645051c`, `024d3d8`, `6e4f8da`, `3f588b9`, `fd2b734`, `6982d24`).
>   - **Error Classification (Finding 4)**: Expanded `is_network_offline_io` in `src/error.rs` to match 9 standard `std::io::ErrorKind` variants (`TimedOut`, `ConnectionReset`, `ConnectionAborted`, `NotConnected`, `BrokenPipe`, `NetworkUnreachable`, `HostUnreachable`, `NetworkDown`, `ConnectionRefused`) before checking raw Win32 error codes, preventing spurious eviction of failed network items from worker retry queues.
>   - **Block Size Safety & Invariants (Finding 3)**: Strongly typed `DirtyBlockRange.block_size` as `NonZeroU64` in `src/sync/delta.rs`. Added `new_nonzero(NonZeroU64)`, fallible `try_new(u64) -> Result<Self, SyncError>`, `TryFrom<u64>`, `Default`, and preserved backwards-compatible panicking `new(u64)`. Added `TargetSyncConfig::block_size_nonzero(&self) -> NonZeroU64` in `src/config.rs` defaulting safely to 64KB on zero values.
>   - **Worker Liveness & Anti-Spin (Finding 1)**: Extracted `calculate_worker_poll_timeout` in `src/sync/worker.rs`. Evaluated `can_drain = reachability.is_dest_online() && source_connectivity.is_online() && !network_offline_detected;` before poll and clamped timeout to 1s when offline with pending queue items, eliminating 100% CPU busy-spinning during network outages.
>   - **Daemon Startup SMB Decoupling (Finding 2)**: Modified `SyncDaemon::validate_target_loops` in `src/daemon.rs` to call `resolver.try_resolve_unc_path` instead of blocking `try_resolve_alternate_path`, eliminating 30–90+ second UI thread hangs during offline share validation.
>   - **SMB Root Reparse Caching (Finding 5 Part A)**: Guarded root destination directory reparse checks in `src/sync/path_safety.rs` with `if !verified_dirs.contains(dest_dir)` and cached `dest_dir` on validation across both Windows and non-Windows cfgs, eliminating up to 50,000 redundant root SMB RPC stat calls per full scan.
>   - **SQLite Signature Cache Hit Fast-Path (Finding 5 Part B)**: In `LocalSyncEngine::sync_file_to_dest_core` (`src/sync/engine.rs`), when `dest_meta.is_some()` and the local `file_record` matches source size and mtime, skipped destination re-verification and delta hashing immediately (`Ok(None)`).
>   - **DirtyRange Mutex Elimination via RAII Lease Pool (Finding 6)**: Replaced struct-level `dirty_range: Mutex<DirtyBlockRange>` in `LocalSyncEngine` with `dirty_range_pool: Mutex<Option<DirtyBlockRange>>`. Introduced RAII `DirtyRangeLease<'a>` implementing `Deref`, `DerefMut`, and `Drop` (returning reset buffers and preserving highest capacity), allowing multi-gigabyte delta transfers and Blake3 hashing without holding any mutex during I/O.
>   - Verified across full test suite: 272 passed (222 lib unit, 3 main unit, 12 integration, 8 property, 20 snapshot, 7 doctests), 0 failed, 1 ignored. Clean formatting (`cargo fmt --check`) and zero warnings (`cargo clippy -- -D warnings`).
> * **New Constraints:**
>   - `DirtyBlockRange` MUST enforce `NonZeroU64` block size to prevent divide-by-zero panics and file corruption at offset 0.
>   - Sync worker poll timeouts MUST be clamped to at least 1s whenever `can_drain` is false to prevent CPU busy-spinning.
>   - Daemon startup target loop validation MUST NEVER invoke blocking network discovery methods (`try_resolve_alternate_path`).
>   - Delta sync streaming MUST NOT hold shared mutex locks across file read, hashing, or network write loops.
> * **Pruned:**
>   - Struct-level `dirty_range: Mutex<DirtyBlockRange>` lock contention across delta sync eliminated.
>   - Unchecked `u64` block size division and modulo in `DirtyBlockRange` eliminated.
>   - Raw OS error code limitation in `is_network_offline_io` eliminated.
>   - Blocking SMB resolution during daemon loop validation eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Documentation Synchronization & Behavioral Contract Harmonization (`/update-doc`)
> * **Changes:**
>   - Synchronized rustdoc comments across all modified public APIs: documented `# Arguments` and `# Returns` for `is_network_offline_io` (`src/error.rs`), `# Returns` for `TargetSyncConfig::block_size_nonzero` (`src/config.rs`), `# Arguments`, `# Returns`, `# Errors`, and `# Panics` for `DirtyBlockRange` constructors (`src/sync/delta.rs`), and `# Returns` for `LocalSyncEngine::acquire_dirty_range_lease` (`src/sync/engine.rs`).
>   - Updated `spec.md` with current verification hash (`> Last verified against: 15e2d61`) and date (`2026-09-10`).
>   - Synchronized `spec.md` Public API tables: added `TargetSyncConfig::block_size_nonzero`, `DirtyBlockRange` constructors and helpers (`new`, `new_nonzero`, `try_new`, `block_size_nonzero`), `LocalSyncEngine::acquire_dirty_range_lease`, and `verify_destination_not_reparse_cached`. Clarified `validate_target_loops` non-blocking UNC semantics and `is_network_offline_io` `ErrorKind` mappings.
>   - Added Data Model contracts for `DirtyBlockRange` (strongly typed `NonZeroU64`) and `DirtyRangeLease<'a>` (RAII zero-lock checkout pool).
>   - Added 4 new behavioral scenarios to `spec.md`: zero block size validation, worker offline anti-spin timeout, SQLite cache hit fast-path, and RAII lease zero-lock delta streaming.
>   - Synchronized automated testing metrics in `spec.md`: 273 automated tests (223 unit tests in `src/lib.rs`, 3 tests in `src/main.rs`, 12 integration tests, 8 property tests, 20 snapshot tests, 7 doc-tests).
> * **New Constraints:**
>   - All new or modified public functions and structs must maintain rustdoc `# Arguments`, `# Returns`, `# Errors`, and `# Panics` sections per `doc-rules.md §1`.
>   - Behavioral scenarios in `spec.md` must be updated whenever public API contracts or error semantics change.
> * **Pruned:**
>   - Stale test counts, undocumented public API items, and verification hash drift in `spec.md` resolved and synchronized.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Safety Invariants Enforcement in `TargetSyncConfigBuilder` (Qualitative Review Finding 1 Remediation)
> * **Changes:**
>   - Updated `TargetSyncConfigBuilder::build` in `src/config.rs` to enforce:
>     - Recursive sync loop detection (`is_same_or_descendant` check between source and destination in both directions).
>     - Non-zero debounce duration (`debounce_seconds > 0`).
>     - Non-zero retry interval (`retry_interval_seconds > 0`).
>     - Block threshold ordering (`block_sync_threshold_bytes >= block_size_bytes`).
>   - Added TDD unit test suite `test_target_sync_config_builder_validation_invariants` in `src/config.rs` testing all failure modes.
>   - Verified all 273 automated tests pass with 0 failures, 0 warnings.
> * **New Constraints:**
>   - Standalone domain builder `TargetSyncConfigBuilder` MUST enforce identical path containment and timeout positivity invariants as top-level `Config::validate()`.
> * **Pruned:**
>   - Validation asymmetry between `Config::validate()` and `TargetSyncConfigBuilder::build()` permanently eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Documentation sync for `TargetSyncConfig` / `TargetSyncConfigBuilder` Public APIs (`/update-doc`)
> * **Changes:**
>   - Enriched rustdoc comments in `src/config.rs` with `# Arguments`, `# Returns`, and `# Errors` sections for `TargetSyncConfig::builder`, `TargetSyncConfig::new`, `TargetSyncConfigBuilder::new`, and `TargetSyncConfigBuilder::build`.
>   - Synchronized `spec.md` verification baseline hash against source commit `57e1de8`.
>   - Verified 100% metadata alignment across `Cargo.toml [package.description]`, `src/lib.rs //!`, and `README.md` overview.
>   - Verified 0 rustdoc warnings across all 7 doc-tests.
> * **New Constraints:**
>   - All builder and constructor public APIs must document explicit parameter types, return values, error triggers, and invariants per `doc-rules.md §1`.
> * **Pruned:**
>   - Undocumented builder parameters and missing `# Returns` sections resolved.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Architecture Documentation Synchronization (`/architecture` Audit Remediation)
> * **Changes:**
>   - Updated `architecture.md § 4 Project Layout` to replace the outdated monolithic `src/sync.rs` entry with the decomposed `src/sync/` directory tree (9 submodules: `mod.rs`, `archive.rs`, `delta.rs`, `engine.rs`, `mock.rs`, `path_safety.rs`, `scanner.rs`, `small_file.rs`, `worker.rs`) and updated test suite metrics (12 integration tests, 20 snapshot tests).
>   - Updated `architecture.md § 5 Module Boundaries` to document `TargetSyncConfigBuilder`'s newly enforced validation invariants (recursive sync loop containment, interval positivity, and block threshold bounds) and corrected `MockSyncEngine` location to `src/sync/mock.rs`.
>   - Updated `architecture.md § 10 Testing Strategy` to reflect 274 total automated tests (224 unit tests in `src/lib.rs`, 3 in `src/main.rs`, 12 integration tests, 8 property tests, 20 snapshot tests, and 7 doc-tests) and enumerated `src/sync/` co-located unit test submodules.
> * **New Constraints:**
>   - `architecture.md` must continuously reflect decomposed module layouts and exact automated test suite counts.
> * **Pruned:**
>   - Stale references to monolithic `src/sync.rs`, 197 test count metric, and outdated integration/snapshot counts eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Critical Findings Remediation (Archive Pruning, DebounceQueue Action Replacement & Compaction, Fallible ConfigBuilder Validation, DirtyBlockRange Zero-Panic Constructor)
> * **Changes:**
>   - **`src/sync/archive.rs`**: Anchored `prune_archive` retention on `{timestamp}_` filename prefix with nested directory timestamp inheritance, preventing premature deletion of older files preserved across Windows NTFS renames.
>   - **`src/sync/worker.rs`**: Updated `DebounceQueue::enqueue_sync` and `enqueue_delete` to permit action replacement on already-tracked paths regardless of queue capacity (`pending_count >= max_capacity`), preventing stale deletions of modified files. Implemented `compact_heaps()` to prune dead entries from min-heaps during bursts.
>   - **`src/config.rs`**: Converted `ConfigBuilder::build(self)` to fallible `Result<Config, SyncError>` enforcing all validation invariants (`debounce_seconds > 0`, `retry_interval_seconds > 0`, `0 < block_size_bytes <= 64MB`, `block_sync_threshold_bytes >= block_size_bytes`, destination presence, and source/dest containment). Preserved `build_unvalidated()` for isolated test fixtures; migrated 57 call sites across workspace.
>   - **`src/sync/delta.rs` & `src/sync/engine.rs`**: Converted `DirtyBlockRange::new` to require `std::num::NonZeroU64`, eliminating `.expect()` panics. Added fallible `try_new(u64) -> Result<Self, SyncError>`.
>   - **Test Suite**: Added 10 new unit and property tests. Full test suite expanded from 268 to 278 automated tests (228 lib + 3 bin + 12 integration + 8 property + 20 snapshot + 7 doc-tests). All exit 0.
> * **New Constraints:**
>   - `ConfigBuilder::build()` is fallible and must be handled with `?` or `.unwrap()` in production/tests. Invalid configs must use `build_unvalidated()` only when explicitly testing invalid states.
>   - `DirtyBlockRange::new()` requires `NonZeroU64`. Use `DirtyBlockRange::try_new(u64)` for dynamic values.
>   - `DebounceQueue` must allow action replacement for existing paths even at max capacity.
>   - Archive retention anchors must parse the filename timestamp prefix rather than querying filesystem creation timestamps (`btime`).
> * **Pruned:**
>   - Runtime panic in `DirtyBlockRange::new(0)` eliminated.
>   - Stale deletion bug on full `DebounceQueue` eliminated.
>   - Unchecked builder construction bypasses eliminated.
>   - Premature archive pruning on NTFS renames eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Documentation sync for Critical Findings Remediation (`/update-doc`)
> * **Changes:**
>   - Enriched rustdoc comments in `src/config.rs` with `# Returns`, `# Errors`, and runnable doctests for `ConfigBuilder::build`, `build_unvalidated`, and `try_build`.
>   - Fixed rustdoc intra-doc link resolution for `TargetSyncConfig` in `src/db.rs`.
>   - Synchronized `spec.md` verification baseline hash against source commit `78822d4`.
>   - Synchronized `spec.md` behavioral contracts and scenarios for `ConfigBuilder`, `DirtyBlockRange`, `DebounceQueue`, archive pruning retention, and test metrics (279 tests).
>   - Verified 0 warnings on `cargo doc --no-deps` and all 8 doc-tests passing (`cargo test --doc`).
> * **New Constraints:**
>   - `ConfigBuilder::build()` doctests must use fallible `?` error propagation.
> * **Pruned:**
>   - Stale panicking `DirtyBlockRange::new` contract and outdated `ConfigBuilder::build` signature in `spec.md` removed.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Qualitative Code Review Remediations (Archive Junction Guard, Truncated Destination File Repair, Two-Phase Locking, SyncWorkerRunner State Machine Extraction, Watcher Buffer Overflow Recovery, Error Classification, Reparse Cache Invalidation)
> * **Changes:**
>   - **`src/sync/archive.rs`**: `prune_archive` verifies that `archive_dir` is not an NTFS junction/reparse point or symlink before traversal via `fs::symlink_metadata`, returning `Err(SyncError::Validation)` to guard against arbitrary file deletions outside backup root. Evicts parent from `verified_dirs` on file deletion in `delete_file_from_dest`.
>   - **`src/sync/engine.rs`**: `sync_file_to_dest_core` verifies destination file size (`dest_size == src_size`) and modification timestamp (`abs_diff <= 2000`ms) against source metadata, actively repairing truncated/corrupted target files instead of skipping. Implemented two-phase locking in `verify_destination_cached` to drop mutex lock across remote SMB `symlink_metadata` calls. Added `invalidate_verified_dirs` to `trait SyncEngine` and prefix eviction `evict_verified_dir` on `LocalSyncEngine`.
>   - **`src/sync/worker.rs`**: Extracted discrete, testable `SyncWorkerRunner<E>` state machine with `handle_command` and `tick(now: Instant)` methods, enabling deterministic zero-sleep unit testing of queues, reachability, and retries. Preserved `start_sync_worker` public signature.
>   - **`src/monitor.rs`**: `DirectoryWatcher::handle_watcher_result` detects notify errors (including `ReadDirectoryChangesW` buffer overflow) and automatically dispatches `SyncCommand::TriggerFullScan` to prevent permanently dropped filesystem events.
>   - **`src/error.rs`**: Added `SyncError::is_permanent_validation_failure` and constructors `validation_security` / `validation_invariant` to prevent retry exhaustion on fatal security violations while preserving backoff retries for transient failures. Retained exact `SyncError::Validation(String)` representation for snapshot compatibility.
>   - **Test Suite**: Added 7 new unit tests across co-located modules; test suite expanded to 286 passing tests (235 unit + 3 bin + 12 integration + 8 property + 20 snapshot + 8 doc-tests) with zero warnings and clean formatting.
> * **New Constraints:**
>   - `prune_archive` must never traverse reparse or junction root directories.
>   - Fast-path destination skip checks must compare destination size and timestamp against source.
>   - Cache mutex locks must not be held across network/SMB filesystem calls.
>   - Permanent validation errors must be classified via `is_permanent_validation_failure` to prevent worker retry lockups.
> * **Pruned:**
>   - Monolithic untested thread loop in `start_sync_worker` replaced by `SyncWorkerRunner`.
>   - Unchecked destination file size skip bug eliminated.
>   - Reparse point cache stale substitution window eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Documentation sync for Qualitative Review Remediations (`/update-doc`)
> * **Changes:**
>   - Enriched rustdoc comments in `src/error.rs` for `SyncError::validation_security`, `validation_invariant`, and `is_permanent_validation_failure` with `# Arguments`, `# Returns`, and runnable doc-tests (11 total doc-tests passing).
>   - Enriched rustdoc comments in `src/sync/engine.rs` for `SyncEngine::invalidate_verified_dirs` and `LocalSyncEngine::evict_verified_dir`.
>   - Enriched rustdoc comments in `src/sync/worker.rs` for `WorkerTickOutcome` and `SyncWorkerRunner` methods (`new`, `handle_command`, `tick`).
>   - Synchronized `spec.md` baseline hash to commit `7316ffc`, added Sync, Monitor, and Error module APIs and behavioral scenarios, and updated test metrics to 289 automated tests.
> * **New Constraints:**
>   - All error classification helpers and worker state machine methods must maintain runnable doc-tests with zero warnings on `cargo doc --no-deps`.
> * **Pruned:**
>   - Outdated test suite counts and undocumented worker runner APIs eliminated.







