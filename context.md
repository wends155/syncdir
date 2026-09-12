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

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Architecture Documentation Synchronization (`architecture.md`)
> * **Changes:**
>   - **Section 5 (`Module Boundaries`)**: Documented `SyncWorkerRunner<E>` state machine, `SyncEngine::invalidate_verified_dirs` trait method, `LocalSyncEngine::evict_verified_dir`, root junction safety checks in `prune_archive`, active destination truncation/corruption repair in `sync_file_to_dest_core`, and watcher buffer overflow error recovery in `monitor` dispatching `SyncCommand::TriggerFullScan`.
>   - **Section 8 (`Error Handling Strategy`)**: Documented `SyncError::is_permanent_validation_failure()` classification and semantic constructors (`validation_security`, `validation_invariant`) for worker queue eviction without retry exhaustion.
>   - **Section 10 (`Testing Strategy`)**: Synchronized test suite metrics to 289 passing automated tests (235 unit tests in `src/lib.rs`, 3 in `src/main.rs`, 12 integration, 8 property, 20 snapshot, 11 doc-tests) and refreshed submodule unit test descriptions.
>   - **Section 14 (`Known Constraints & Technical Debt`)**: Documented two-phase locking in `verify_destination_cached` for SMB latency optimization and dynamic reparse cache invalidation hooks.
> 📝 **Context Update (2026-09-10):**
> * **Feature:** Syncdir Architecture Decoupling & Module-Scoped Refactoring (All 15 findings from `review_report.md` resolved)
> * **Changes:**
>   - **Strongly-Typed Error Classification (O1)**: Introduced `ValidationKind` enum (`Security`, `ReparsePoint`, `RecursiveLoop`, `Invariant`, `Transient`) in `src/error.rs`. Refactored `SyncError::Validation { kind, message }` with exact display format `\"Validation error: {message}\"`. Refactored `is_permanent_validation_failure(&self) -> bool` to use pattern matching rather than fragile substring comparisons, preserving all 20 golden snapshots byte-for-byte.
>   - **Leaf Utility Relocation (O2)**: Relocated `is_same_or_descendant`, `system_root`, and `open_path` to `src/path_util.rs`. Decoupled `daemon.rs` from `tray.rs` and decoupled `tray.rs` from `config.rs`.
>   - **Persistence & Configuration Decoupling (O3)**: Completely severed `src/db.rs` imports of `src/config.rs`. `StoreConfig` is now a pure configuration value object constructed via `StoreConfig::new(block_size, threshold)`. Implemented `TryFrom<&Config> for StoreConfig` in `src/config.rs`. Implemented `HashStore` for `Arc<S>` and `&S` in `src/db.rs`.
>   - **Invariant Enforcement in Config (O5)**: Refactored `TargetSyncConfig::from_config` to route through `TargetSyncConfigBuilder` returning `Result<Self, SyncError>`, enforcing recursive containment, non-empty paths, and positive timeouts. Deprecated `Config::resolved_source_dir` in favor of `Config::source_dir`.
>   - **`LocalSyncEngine` Decomposition (O6)**: Decomposed God struct partial class pattern across 5 files into 4 collaborating components:
>     - `SmallFileTransferEngine` (`src/sync/small_file.rs`): fast-path small-file streaming and atomic staging.
>     - `DeltaTransferEngine<S: HashStore>` (`src/sync/delta.rs`): in-place delta synchronization, chunk hashing, and dirty range pooling.
>     - `ArchiveManager<S: HashStore>` (`src/sync/archive.rs`): retention-based archive management, timestamped backups, and safe directory pruning.
>     - `DirectoryScanner<S: HashStore>` (`src/sync/scanner.rs`): directory traversal, batch DB persistence, and case-insensitive deletion detection.
>     - `LocalSyncEngine<S>` (`src/sync/engine.rs`): cleanly composes the 4 collaborating structs and implements `SyncEngine` by delegation. Implemented `From<&FileRecord> for FileMetadataSnapshot`.
>   - **Public Interface Hardening & DIP (O4)**: Injected `Arc<dyn NetworkResolver>` into `SyncWorkerContext::new` (with `for_test` constructor). Demoted `SyncWorkerRunner` fields to `pub(crate)`. Demoted standalone free functions in `src/net.rs` to `pub(crate)`. Encapsulated `winit` event proxy in `TrayEventLoop::status_observer(&self) -> Arc<dyn SyncStatusObserver>`, completely decoupling `src/main.rs` from `winit`. Demoted `TrayController` to `pub(crate)`.
>   - **Submodule Encapsulation (O7)**: Encapsulated all `src/sync/` submodules (`archive`, `delta`, `engine`, `mock`, `path_safety`, `scanner`, `small_file`, `worker`) as `pub(crate) mod`. Re-exported required facade types from `syncdir::sync`. Updated integration and doctest imports.
>   - **Full Verification Pipeline (O8)**: 298 total automated tests passing with zero regressions and zero compiler warnings (244 unit tests, 3 bin tests, 12 integration tests, 8 property tests, 20 snapshot tests matching byte-for-byte, 11 doctests).
> * **New Constraints:**
>   - Submodules within `src/sync/` must remain `pub(crate) mod`. External consumers interact exclusively through `syncdir::sync` facade.
>   - Future database access must remain decoupled from `config.rs`.
>   - Permanent validation error detection must use `ValidationKind::is_permanent()`.
>   - `TargetSyncConfig::from_config` must return `Result<TargetSyncConfig, SyncError>`.
> * **Pruned:**
>   - Partial class `LocalSyncEngine` pattern across 5 files eliminated.
>   - Stringly-typed error matching in `is_permanent_validation_failure` eliminated.
>   - Reverse `db -> config` and `config -> db` coupling eliminated.
>   - `main.rs` coupling to `winit` eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Bucket 1: Leaf & Facade Encapsulation Refactoring (`review_report.md` Findings 3, 4, 7, 9, 11, 12, 15, 17)
> * **Changes:**
>   - **DB & Daemon Cache Path Encapsulation (O1)**: Introduced `SqliteHashStore::cache_db_path(app_dir: &Path, target_dest: &Path) -> PathBuf` in `src/db.rs` with dedicated TDD unit test `test_sqlite_cache_db_path`. Removed raw Blake3 hashing and `sigcache_<hex>.db` path formatting from `SqliteEngineFactory::create_engine` in `src/daemon.rs`.
>   - **Config & Daemon Path Coupling Severance (O2)**: Removed `pub use crate::path_util::{is_same_or_descendant, system_root};` from `src/config.rs`. Migrated 12 `is_same_or_descendant` callers in `src/daemon.rs` directly to `crate::path_util::is_same_or_descendant`. Updated `config.rs` unit tests to import `system_root` directly from `crate::path_util`.
>   - **Leaf Module Encapsulation (Net, Monitor, Tray) (O3)**: Demoted 5 internal free functions (`resolve_mapped_drive_unc`, `try_resolve_unc_path`, `establish_smb_connection`, `find_mapped_drive_for_unc`, `try_resolve_alternate_path`) in `src/net.rs` to private `fn` across both Windows and non-Windows targets. Demoted `handle_watcher_result` and `dispatch_event` in `src/monitor.rs` to private `fn`. Deleted dead delegation wrapper `tray::open_path` in `src/tray.rs` and updated `test_tray_open_path_nonexistent` to invoke `crate::path_util::open_path`.
>   - **Sync Subsystem Facade & Zero-Panic Mock (O4)**: Restricted `LocalSyncEngine::acquire_dirty_range_lease` visibility to `pub(crate)`. Replaced all 25 unchecked `.lock().unwrap()` calls in `MockSyncEngine` (`src/sync/mock.rs`) with `.lock().unwrap_or_else(|p| p.into_inner())` to satisfy zero-panic policy. Pruned unused worker internals (`DebounceQueue`, `ReachabilityMonitor`, `calculate_exponential_backoff`, `verify_destination_not_reparse_cached`) from `syncdir::sync` public facade while preserving `is_metadata_up_to_date_raw` for property tests.
>   - **Full Quality Verification Gate (O5)**: Verified zero formatting diffs (`cargo fmt --all -- --check`), zero lint warnings (`cargo clippy --all-targets --all-features -- -D warnings`), and 100% test pass rate across all 299 automated tests (`cargo test --all-features`).
> * **New Constraints:**
>   - Callers generating cache database paths must invoke `SqliteHashStore::cache_db_path` rather than constructing database filenames directly.
>   - `daemon.rs` must import path utilities directly from `crate::path_util`, never through re-exports in `config.rs`.
>   - `MockSyncEngine` mutex locks must recover from poison via `unwrap_or_else(|p| p.into_inner())`.
> * **Pruned:**
>   - Leaked DB filename format and raw Blake3 hashing in `daemon.rs` eliminated.
>   - Phantom dependency of `daemon.rs` on `config.rs` for path operations eliminated.
>   - Dead `tray::open_path` wrapper eliminated.
>   - Unchecked mutex lock panics in `MockSyncEngine` eliminated.
>   - Clutter in `syncdir::sync` crate facade eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Bucket 2: Architectural Decoupling (`daemon` $\longleftrightarrow$ `tray`) (`review_report.md` Findings 1, 6)
> * **Changes:**
>   - **Composition Root Relocation of `DaemonTrayHandler` (O1)**: Moved `DaemonTrayHandler<R: RegistryBackend>` from `src/daemon.rs` into the binary entrypoint `src/main.rs`. Implemented `DaemonTrayHandler` as a private struct connecting UI callbacks to `DaemonHandle`, `RegistryBackend`, and `NetworkResolver`. Added `test_daemon_tray_handler_actions` to `src/main.rs::mod tests` verifying all tray actions in-memory.
>   - **Severed `daemon` Dependency on `tray` & Facade Clean-Up (O2)**: Completely removed `tray` and `startup` imports from `src/daemon.rs`. Deleted `DaemonTrayHandler` and associated tests from `src/daemon.rs`. Promoted `DaemonHandle` to `syncdir::daemon::{DaemonHandle, SyncDaemon}` in `src/lib.rs`. Enriched `DaemonHandle::trigger_full_scan` with `# Errors` doc comments. Retained `use std::path::PathBuf;` in `daemon.rs::mod tests` so `TrackingResolver` cleanly compiles.
>   - **Encapsulated Windowing Subsystem in `tray` (O3)**: Restricted `run_tray`, `create_proxy`, `TargetStatusUpdate`, and `UserEvent` to `pub(crate)` within `src/tray.rs`. Completely hid all `winit` types behind `TrayEventLoop`. Annotated `TrayEventLoop::status_observer` with `#[must_use]`, enriched `new()` and `run()` with `# Arguments`, `# Returns`, and `# Errors` doc comments, and updated `TrayExitReason` doc comment.
>   - **Updated Integration Test Suite (O3)**: Migrated `test_tray_module_compiles` in `tests/integration_tests.rs` from `TrayRunner` (testing `run_tray` accepting raw `winit::event_loop::EventLoop`) to `TrayLoopRunner` (testing `TrayEventLoop::run`), eliminating `winit` references from integration tests.
>   - **Synchronized Architectural Governance (O4)**: Updated `architecture.md §6 Dependency Direction Rules` table and Cycle Resolution Note to record `main` importing `net` and `path_util` as the composition root, and `daemon` being strictly prohibited from importing `tray` or `startup`. Synchronized `architecture.md §13 Module Interaction Graph` Mermaid diagram.
>   - **Full Verification Pipeline (O5)**: Zero formatting diffs (`cargo fmt`), zero lint warnings (`cargo clippy`), and all 299 tests passing (`cargo test --all-features`).
> * **New Constraints:**
>   - `src/daemon.rs` must NEVER import `tray` or `startup`. All UI and startup dispatching is owned by the composition root in `src/main.rs`.
>   - Windowing types (`winit`, `UserEvent`, `TargetStatusUpdate`) must remain encapsulated within `src/tray.rs`. External consumers interact solely with `TrayEventLoop`.
> * **Pruned:**
>   - Prohibited `daemon -> tray` architectural dependency eliminated.
>   - Public exposure of `winit` event types and proxies eliminated.
>   - Dead `startup` imports in `daemon.rs` eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Bucket 3: Config Subsystem Decomposition (`review_report.md` Findings 5, 8, 10, 14)
> * **Changes:**
>   - **Submodule Extraction & Hierarchy (O1, O2, O3, O4, O5, O6)**: Decomposed the 2,257-line monolithic `src/config.rs` into an acyclic module tree under `src/config/`:
>     - `src/config/mod.rs` (560 lines): Central facade, public re-exports, `from_raw_parts` factory methods, Serde DTO conversion bridges (`From<RawConfig>`, `Into<RawConfig>`), and `StoreConfig` `TryFrom` conversions.
>     - `src/config/target.rs`: `VerificationMode`, `TargetRole` (`pub(crate)`), `TargetDir` (self-normalizing path invariant with private `inner: PathBuf`), and `DestinationCollection` (case-insensitive deduplication).
>     - `src/config/validation.rs`: TOML preprocessing (`preprocess_config_toml`, `escape_backslashes_in_quotes`) and numeric bound constants (`MAX_BLOCK_SIZE_BYTES`, `DEFAULT_DEBOUNCE_SECONDS`, `DEFAULT_RETRY_INTERVAL_SECONDS`).
>     - `src/config/raw.rs`: `RawConfig` DTO with `pub(crate)` fields for TOML deserialization/serialization.
>     - `src/config/builder.rs`: `ConfigBuilder` and `TargetSyncConfigBuilder` using `from_raw_parts`, with infallible builder semantics and validation deferred to `validate()` or `build()`.
>     - `src/config/tests.rs`: Ported all 48 comprehensive unit tests using explicit item-level imports, plus 2 new tests validating `StoreConfig` conversions.
>   - **Strict Field Encapsulation**: Tightened all 9 fields of `TargetSyncConfig` (`source_dir`, `dest_dir`, `debounce_seconds`, `block_size_bytes`, `block_sync_threshold_bytes`, `verify_writes`, `verification_mode`, `propagate_deletions`, `archive_retention_days`) from `pub(crate)` to strictly private. Provided getter methods and `with_verify_writes` mutation helper.
>   - **Persistence Bridge Formalization**: Implemented `TryFrom<&Config> for StoreConfig` and `TryFrom<&TargetSyncConfig> for StoreConfig` in `src/config/mod.rs`, cleanly validating non-zero block sizes via `StoreConfig::new`.
>   - **Consumer Adaptation**: Updated `src/sync/small_file.rs:370` test from struct update syntax on private fields to `target_cfg.with_verify_writes(true)`.
>   - **Full Quality Verification Gate**: Verified zero formatting diffs (`cargo fmt`), zero lint warnings (`cargo clippy -D warnings`), and 100% test pass rate with 301 passing tests (`cargo test --all-features`).
> * **New Constraints:**
>   - Submodules within `src/config/` must maintain acyclic dependencies (`mod.rs` $\to$ submodules). Sibling submodules must not import each other directly unless necessary.
>   - `TargetSyncConfig` fields must remain strictly private; consumers must use accessors or `TargetSyncConfigBuilder`.
>   - Database layer `StoreConfig` construction from configuration must use `TryFrom` conversions in `src/config/mod.rs`.
> * **Pruned:**
>   - 2,257-line monolithic `src/config.rs` eliminated.
>   - Public / crate-private field leakage on `TargetSyncConfig` eliminated.
>   - Unchecked direct struct construction of `TargetSyncConfig` by consumers eliminated.

---

> 📝 **Context Update (2026-09-10):**
> * **Feature:** Bucket 4: Sync Subsystem Restructuring (`review_report.md` Findings 6, 8, 13, 14)
> * **Changes:**
>   - **DebounceQueue Drain Safety & Zero-Panic Mandate (O1)**: Replaced unchecked `.pop().unwrap()` in `DebounceQueue::drain_ready_syncs` and `drain_ready_deletes` with `if let Some(...)` bounds in `src/sync/worker.rs`. Added unit test `test_debounce_queue_drain_safety`.
>   - **Reparse Verification Lock Release & Cache Poisoning Prevention (O1)**: Rewrote `verify_destination_cached` in `src/sync/engine.rs` to inspect ancestor cache under a brief lock, release the lock during remote filesystem I/O, verify `meta.is_dir()`, stop caching on `NotFound`, and re-acquire the lock solely for verified directories. Gated unused `verify_destination_not_reparse_cached` under `#[cfg(test)]` in `src/sync/path_safety.rs`.
>   - **Atomic Coordination Consolidation (O2)**: Consolidated all cross-module coordination methods (`run_cancellable_full_scan_impl`, `flush_record_batch`, `delete_file_from_dest`, `prune_destination_archive`, `archive_dest_file_only`, `sync_delta_large_file_core`, `sync_delta_large_file`, `sync_small_file_core`, `sync_small_file`) into `src/sync/engine.rs`. Stripped all `impl LocalSyncEngine` blocks from `scanner.rs`, `archive.rs`, `delta.rs`, and `small_file.rs`, completely eliminating cross-submodule fragmentation without compiler duplicate definition conflicts.
>   - **Unit Test Decoupling & Relocation (O3)**: Decoupled unit tests in `src/sync/small_file.rs`, `src/sync/delta.rs`, `src/sync/archive.rs`, and `src/sync/scanner.rs` to test `SmallFileTransferEngine`, `DeltaTransferEngine`, `ArchiveManager`, and `DirectoryScanner` directly. Relocated all 20 end-to-end integration orchestration tests into `src/sync/engine.rs::mod tests`.
>   - **Engine Field Encapsulation (O3)**: Encapsulated all 8 fields of `LocalSyncEngine` (`db`, `config`, `resolved_dest`, `verified_dirs`, `small_file_engine`, `delta_engine`, `archive_manager`, `scanner`) to strictly private.
>   - **Worker Context Encapsulation & Builder Pattern (O4)**: Demoted all 9 fields of `SyncWorkerContext` in `src/sync/worker.rs` to `pub(crate)`. Introduced `SyncWorkerContextBuilder` enforcing `max_pending_queue > 0` validation (`SyncError::Validation { kind: ValidationKind::Invariant, .. }`). Added read-only borrowing getters and fluent setters (`with_resolver`, `with_cancellation`, `with_max_pending_queue`). Updated `tests/integration_tests.rs` to use fluent chaining.
>   - **Trait Contract Documentation & Quality Verification Gate (O5)**: Re-exported `SyncWorkerContextBuilder` in `src/sync/mod.rs`. Enriched doc comments and error contracts on `SyncEngine` trait and methods in `src/sync/engine.rs`. Verified zero formatting diffs (`cargo fmt`), zero lint warnings (`cargo clippy -D warnings`), and 100% test pass rate with 302 passing tests (`cargo test --all-features`).
> * **New Constraints:**
>   - Submodules within `src/sync/` (`scanner.rs`, `archive.rs`, `delta.rs`, `small_file.rs`) must remain specialized leaf transfer engines. `LocalSyncEngine` coordination logic must live exclusively in `src/sync/engine.rs`.
>   - `SyncWorkerContext` fields must remain `pub(crate)` with access mediated by borrowing getters. Construction should prefer `SyncWorkerContext::builder` or fluent configuration helpers.
>   - Directory ancestor verification caching must never hold mutex locks across blocking network filesystem calls.
> * **Pruned:**
>   - Fragmented `impl LocalSyncEngine` blocks across leaf submodules eliminated.
>   - Public field exposure on `LocalSyncEngine` eliminated.
>   - Unchecked `.pop().unwrap()` in debounce queue drainage eliminated.
>   - Lock contention and cache poisoning risks in destination verification eliminated.

---

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Phase 1: Critical Bug, Concurrency, and Security Fixes
> * **Changes:**
>   - **Worker Drain Shutdown Safety (O1)**: Labeled outer loop `'worker: loop` and broke out of `'worker` on `!runner.handle_command(cmd)` in inner `while let Ok(cmd) = runner.context.rx.try_recv()` drain loop in `src/sync/worker.rs`.
>   - **Full Scan Underflow Guard (O2)**: Calculated `remaining = source_files.len().saturating_sub(synced_count.saturating_add(failed_count))` in `LocalSyncEngine::run_cancellable_full_scan_impl` logging in `src/sync/engine.rs`.
>   - **Destination TOCTOU Symlink Hijack Defense (O3)**: Used `OpenOptionsExt::custom_flags(0x0020_0000)` (`FILE_FLAG_OPEN_REPARSE_POINT`) and verified `(meta.file_attributes() & 0x400) == 0` in `DeltaTransferEngine::sync_delta_large_file_core` in `src/sync/delta.rs`.
>   - **Immediate Eviction on Permanent Failures (O4)**: Dispatched `SyncError::validation_reparse` on junction/symlink detections and `SyncError::validation_security` on path traversals across `path_safety.rs`, `engine.rs`, and `archive.rs`.
>   - **Path Normalization & Hierarchy Containment (O5)**: Guarded empty base paths in `is_same_or_descendant` and restructured trailing backslash trimming in `normalize_path` in `src/path_util.rs`.
>   - **Verification Builder Synchronization & Panic Elimination (O6)**: Synchronized `verify_writes` with `verification_mode` in `TargetSyncConfigBuilder` and `ConfigBuilder`. Replaced `.expect()` with compile-time const default in `block_size_nonzero`. Made `DirtyRangeLease` deref methods panic-free.
>   - **Archive Ancestor Defense & Sampled Optimization (O7)**: Audited intermediate subpath directories under `.syncdir_archive/` with `verify_destination_not_reparse`. Optimized `VerificationMode::Sampled` in `SmallFileTransferEngine` to avoid unnecessary full-file readbacks.
> * **New Constraints:**
>   - All file opens on potentially unprivileged target paths must specify `FILE_FLAG_OPEN_REPARSE_POINT` on Windows.
>   - Reparse and traversal errors must use `ValidationKind::ReparsePoint` / `ValidationKind::Security` to enable instant worker queue eviction.
> * **Pruned:**
>   - Drained shutdown hang in sync worker eliminated.
>   - Full scan arithmetic underflow panic on network disconnect eliminated.
>   - Production code `.expect()` calls in `TargetSyncConfig` and `DirtyRangeLease` eliminated.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Phase 2: Leaf & Trait Decoupling (`review_report.md` Findings 1, 2, 3, 4, 7, 9, 11)
> * **Changes:**
>   - **Path Util Purification & Shell Execution Relocation (O1)**: Relocated `open_path` to `src/tray.rs` as `pub(crate) fn open_path`. Retained self-contained deprecation shim in `src/path_util.rs` without importing `tray`. Removed orphaned `test_system_root`.
>   - **Leaf Component Decoupling via `src/sync/types.rs` (O2)**: Extracted `src/sync/types.rs` declaring `FileSyncTask<'a>`, `safe_epoch_duration_millis`, and `safe_modified_millis`. Severed circular imports from `delta.rs` and `small_file.rs` to `engine.rs`.
>   - **FileRecord Domain Encapsulation (O3)**: Encapsulated `FileRecord` in `src/db.rs` with private fields, `file_size: u64`, constructor `FileRecord::new`, and getters (`.relative_path()`, `.file_size()`, `.last_modified()`, `.id()`, `.is_tracked()`).
>   - **TargetSyncConfig Domain Symmetry (O4)**: Restored `source_dir: TargetDir` in `TargetSyncConfig`, provided `.source_dir() -> &Path` and `.source_target_dir() -> &TargetDir`, updated `TargetSyncConfigBuilder` to hold and validate `TargetDir` without extra clones.
>   - **FileWatcher & WatcherFactory Trait Abstractions (O5)**: Abstracted directory watching in `src/monitor.rs` behind `pub trait FileWatcher: Send + 'static` and `pub trait WatcherFactory: Send + Sync + 'static`. Injected `Arc<dyn WatcherFactory>` into `SyncDaemon` with `RecommendedWatcherFactory` as default. Added `watcher_running(&self) -> bool` and removed dead `command_tx(&self)`.
>   - **Watcher Error Modularization (O6)**: Defined `pub enum WatcherError` in `src/error.rs` (wrapping `notify::Error`, `PathNotFound`, `ChannelDisconnected`, `Other`), re-exported in `src/monitor.rs`. Transparently wrapped in `SyncError::Watcher(#[from] WatcherError)` preserving causal source chains.
>   - **Daemon Shutdown Panic Logging & Event Coordination (O7)**: Added `join_thread_and_log_panic` safely extracting downcasted panic strings across worker, watcher, and broadcaster threads during `perform_shutdown`. Added 100ms interval shutdown polling in watcher coordinator loop.
>   - **Quality Verification Gate**: 320 passing tests (zero failures), zero clippy warnings, and clean formatting check.
> * **New Constraints:**
>   - Leaf transfer engines (`delta.rs`, `small_file.rs`) must import types from `src/sync/types.rs`, never from `engine.rs`.
>   - Direct access to `FileRecord` fields is prohibited; consumers must use getter methods.
>   - Directory watchers must be instantiated via `WatcherFactory` to allow mock injection.
> * **Pruned:**
>   - Inverted circular dependencies from `delta.rs` / `small_file.rs` to `engine.rs` eliminated.
>   - Dead `command_tx` method on `SyncDaemon` eliminated.
>   - Raw panic leaks during daemon thread shutdown eliminated.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Phase 3: Performance & Caching Refactoring (`review_report.md` Findings 12, 13, 14, 15, 17)
> * **Changes:**
>   - **Relative-Depth Aware ReparseCache (O1)**: Implemented `ReparseCache` in `src/sync/path_safety.rs` under a single `std::sync::RwLock` separating shallow ancestors (relative depth $\le 3$ retained up to 50,000 entries) and deep ancestors (retained up to 10,000 entries with automatic oldest eviction). Added descendant prefix eviction `evict_dir`.
>   - **Zero-Allocation Superscript Normalization (O2)**: Implemented `normalize_superscripts_cow` returning `Cow::Borrowed` on ASCII paths and allocating `Cow::Owned` only when unicode superscripts exist. Integrated into `is_safe_relative_path`, eliminating 10 unconditional string allocations per path component.
>   - **Engine Shared Caching (O3)**: Integrated `&ReparseCache` across `verify_destination_not_reparse_cached` and `verify_source_not_reparse_cached`. Replaced `verified_dirs` in `LocalSyncEngine` with shared `Arc<ReparseCache>`, caching source path reparse validation in `sync_file_to_dest_core`.
>   - **Bounded Candidate Scanning in Archive Pruning (O4)**: Capped candidate traversal in `prune_archive` to `MAX_ARCHIVE_PRUNE_CANDIDATES = 5000` files to bound filesystem traversal memory and I/O.
>   - **Worker Debounce Batching & Fallback (O5)**: Added `SyncEngine::sync_file_to_dest_staged` and `flush_staged_syncs`. Updated `SyncWorkerRunner::tick` to stage up to 500 file transfers per debounce cycle. Enhanced `LocalSyncEngine::flush_record_batch` to fall back to individual `save_file` calls upon SQLite batch failure.
>   - **Periodic Archive Prune Telemetry (O6)**: Added structured `tracing::warn!` logging on periodic archive prune failures in `SyncWorkerRunner::tick`.
>   - **Quality Verification Gate**: 329 passing tests (zero failures, 1 ignored), zero clippy warnings, and clean formatting check. Commit `720213f`.
> * **New Constraints:**
>   - All reparse point ancestor checks must reuse the shared `ReparseCache`.
>   - Worker file sync transfers must stage updates via `sync_file_to_dest_staged` and batch commits via `flush_staged_syncs`.
>   - SQLite batch saves must always fall back to individual record saves on failure.
> * **Pruned:**
>   - Unbounded memory consumption in `verified_dirs` cache eliminated.
>   - Unconditional per-component string allocations in `is_safe_relative_path` eliminated.
>   - Per-file individual SQLite disk transaction overhead in worker debounce loop eliminated.
>   - Unbounded candidate discovery in archive pruning eliminated.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Phase 4: FullScanCoordinator Decomposition, RelativePath Domain Newtype, and Final Code Quality Hardening (`review_report.md` Findings 1, 2, 4, 8, 10, 15, 16)
> * **Changes:**
>   - **`RelativePath` Domain Newtype (O1)**: Introduced `RelativePath` in `src/path_util.rs` (re-exported in `src/sync/types.rs`), enforcing `is_safe_relative_path`, forward-slash canonicalization, rejection of traversal/devices/trailing whitespace/ADS, and Serde deserialization invariant enforcement via `try_from`.
>   - **`FileRecord` Encapsulation & Strongly-Typed `SyncCommand` (O2)**: Updated `FileRecord` to hold `relative_path: RelativePath`. Strongly typed `SyncCommand::SyncFile(RelativePath)` and `SyncCommand::DeleteFile(RelativePath)`, migrating all engine, monitor, worker, and test call sites. Updated `SqliteHashStore` to convert from/to `RelativePath`.
>   - **`FullScanCoordinator` Extraction & Zero-Allocation Lookups (O3)**: Decomposed `LocalSyncEngine::run_cancellable_full_scan_impl` into `FullScanCoordinator` (`src/sync/full_scan.rs`), reducing cyclomatic complexity from 135 to <15 per stage. Introduced `NormalizedCaseFoldedPath` for zero-allocation Windows case-insensitive map lookups.
>   - **ArchiveManager `ReparseCache` Integration (O4)**: Integrated `Arc<ReparseCache>` into `ArchiveManager::archive_dest_file_only` across production and test call sites via `verify_destination_not_reparse_cached`.
>   - **Boundary Deduplication & `TargetDir` Validation (O5)**: Centralized sync loop containment into `validate_sync_boundaries` in `src/config/validation.rs`. Introduced `TargetDir::try_new` validating drive and UNC roots; deprecated `TargetDir::new` and `Config::dest_dirs`.
>   - **Fluent `SyncDaemonBuilder` & Context Construction (O6)**: Implemented `SyncDaemonBuilder` consolidating lifecycle construction. Removed dead `SyncWorkerContext::for_test` and enforced `SyncWorkerContextBuilder`. Added manual structured tracing spans (`sync_file`, `full_scan`, `archive_file`) in `src/sync/engine.rs`.
>   - **Retirement of `is_metadata_up_to_date_raw` & Integration Regression (O7)**: Deprecated `is_metadata_up_to_date_raw` in favor of `FileMetadataSnapshot::is_up_to_date`. Migrated property tests and internal callers. Updated `architecture.md § 6` allowing `db -> path_util`.
>   - **Quality Verification Gate**: 343 passing tests (zero failures, 1 ignored), zero clippy warnings (`-D warnings`), and clean formatting check across the entire workspace.
> * **New Constraints:**
>   - Relative file paths across domain models (`FileRecord`, `SyncCommand`) must use `RelativePath`.
>   - `TargetDir` construction must use `TargetDir::try_new` to validate root and syntax invariants.
>   - Full scans must execute through `FullScanCoordinator`.
>   - Background daemon configuration must use `SyncDaemonBuilder`.
> * **Pruned:**
>   - Primitive string/PathBuf representation for relative file paths eliminated.
>   - 216-line monolithic `run_cancellable_full_scan_impl` method eliminated.
>   - Per-iteration string allocation during Windows deletion reconciliation eliminated.
>   - Duplicate sync loop boundary validation algorithms eliminated.
>   - Telescoping constructors on `SyncDaemon` and unchecked `for_test` worker context eliminated.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Post-Phase 4 Documentation Synchronization & Architecture Audit (`/update-doc`, `/architecture`)
> * **Changes:**
>   - **`spec.md` Behavioral Synchronization**: Realigned all behavioral contracts against verified commit `9fa32e6` (`> Last verified against: 9fa32e6`). Documented contracts for `TargetDir::try_new`, `DestinationCollection`, `RelativePath` domain invariants, `FileRecord` encapsulation, `FullScanCoordinator`, `ReparseCache`, `SafeModifiedMillis`, `SyncDaemonBuilder`, and `WatcherFactory`.
>   - **`README.md` API Surface Update**: Updated API surface listing to reflect all modern module exports (`TargetDir`, `SyncDaemonBuilder`, `FileRecord`, `WatcherFactory`, `RelativePath`, `ReparseCache`, `FullScanCoordinator`).
>   - **Architecture Audit & Recommendations Report**: Conducted a comprehensive audit of `architecture.md` against the 16 required sections in `.agents/rules/architecture-rules.md`. Generated `architecture_recommendations_report.md` documenting layout tree synchronization, dependency rule reconciliation for `monitor` and `startup`, `WatcherError` documentation, test metric updates (343 passing tests), and Mermaid diagram adjustments.
>   - **`architecture.md` Synchronization**: Applied all 5 recommendations to `architecture.md`: added `full_scan.rs` and `types.rs` to layout tree (§ 4), added `path_util` to `monitor` and pruned `config` from `startup` in § 6, documented `WatcherError` domain error isolation in § 8, updated test suite metrics to 343 tests and 5 in-memory mocks in § 10, and synchronized Mermaid Module Interaction Graph (`db --> path_util & error`, `monitor --> sync & path_util & error`, `startup --> error`) in § 13. Committed as `f736f7c`.
>   - **Quality Verification Gate**: 343 passing tests (zero failures, 1 ignored), zero clippy warnings (`-D warnings`), and clean formatting check across the entire workspace.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Block 1: Observability & Tracing Modernization (`/build`, `/audit`)
> * **Changes:**
>   - **Declarative Instrumentation (`Cargo.toml`, Subsystem Spans)**: Added `features = ["attributes"]` to `tracing = "0.1"`. Annotated architectural entry points with `#[tracing::instrument]` across `LocalSyncEngine::run_cancellable_full_scan_impl`, `DirectoryScanner::scan_dir_cancellable`, `ArchiveManager::prune_destination_archive`, `SqliteHashStore` (`get_file`, `save_file`, `delete_file`), and Win32 SMB helpers (`resolve_mapped_drive_unc`, `establish_smb_connection`). Kept hot chunk hashing and byte streaming loops span-free.
>   - **CRLF Log Injection Defense (CWE-117)**: Reordered validation so `is_safe_relative_path` strictly precedes span creation in `LocalSyncEngine::sync_file_to_dest_core` and `archive_dest_file_only`. Sanitized relative paths and error messages using Debug representation (`?rel_path`, `{:?}`) across `src/monitor.rs`, `src/path_util.rs`, and `src/sync/archive.rs` to escape `\r\n` control bytes by construction.
>   - **Cross-Thread Span & Dispatcher Propagation**: Propagated parent spans and thread-local dispatchers across spawned OS threads in `start_sync_worker` (`src/sync/worker.rs`) and `SyncDaemon::spawn_watcher_coordinator` / `spawn_command_broadcaster` (`src/daemon.rs`), ensuring background worker logs correlate with daemon contexts.
>   - **Telemetry PII Scrubbing (CWE-532) & Crash-Resilient Panic Flush**: Masked usernames and hostnames in telemetry logs using deterministic Blake3 KDF (`domain: "syncdir telemetry pseudonymization v1"`), outputting `anon-<12-hex>`. Registered `WorkerGuard` in `LOG_WORKER_GUARD` static mutex, added non-blocking log flush in panic hook with double-panic protection (`AtomicBool`), and implemented emergency crash logger `write_emergency_panic_log` to `%APPDATA%\syncdir\logs\crash.log`.
>   - **Pure Helper Purity**: Removed side-effecting log statements from pure lexical and calculation helpers (`safe_modified_millis`, `normalize_path`, `TargetDir::validate`). Added centralized `src/test_support.rs` with `TracingCaptureBuffer` and `with_captured_tracing`.
>   - **Quality Verification Gate**: 347 passing tests across all targets, zero clippy warnings (`-D warnings`), and 100% clean formatting.
> * **New Constraints:**
>   - Untrusted file paths must be validated before entering tracing spans and formatted with Debug specifier `?rel_path` or `{:?}` in error messages to prevent CWE-117 log injection.
>   - Usernames and hostnames in persistent log events must be pseudonymized via `pseudonymize_identifier` to prevent CWE-532 telemetry PII leaks.
>   - Spawned background OS threads must capture and enter the active `parent_span` and `tracing::dispatcher` inside thread closures.
>   - Pure lexical and value helpers must remain side-effect free and emit no tracing events.
> * **Pruned:**
>   - Manual, ad-hoc tracing spans on subsystem boundaries replaced with declarative `#[tracing::instrument]`.
>   - Side-effecting logs in `safe_modified_millis`, `normalize_path`, and `TargetDir::validate` eliminated.
>   - Redundant in-memory tracing buffer test harnesses consolidated into `src/test_support.rs`.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Block 2: Worker Queue & State Resilience (`/build`, `/audit`)
> * **Changes:**
>   - **Transient Missing File Immediate Eviction (Finding 2, O1)**: Added source file existence verification (`!source_dir.join(&path).exists()`) upon `SyncError::Io(e)` with `ErrorKind::NotFound` in `SyncWorkerRunner::tick` (`src/sync/worker.rs`). Deleted transient files are evicted immediately with 0 retries, clearing failure tracking and preventing queue starvation.
>   - **Generic I/O Retry Bound & Observer Notification (Finding 2, O2)**: Bounded generic `SyncError::Io` and `Err(e)` on both sync and delete loops to 10 attempts with exponential backoff before permanent eviction on attempt 11, resetting failure state and notifying `SyncStatusObserver::on_write_verification_failed`.
>   - **Partial Failure Reachability Synchronization (Finding 3, O3)**: In `SyncWorkerRunner::handle_command`, explicitly called `self.reachability.mark_online(...)` on `ScanOutcome::PartialFailure`, unblocking queue processing and synchronizing tray UI state when individual file errors occur during full scans.
>   - **Catch-Up Full Scan Exponential Backoff Throttling (Finding 4, O4)**: Added `catchup_scan_failures: u32` and `next_catchup_scan_attempt: Option<Instant>` to `SyncWorkerState`. `SyncWorkerRunner::tick` enforces backoff on failed catch-up full scans (starting at 10s base), preventing tight CPU spin-loops.
>   - **Event-Driven Watcher Coordinator Shutdown & Structured Telemetry (Findings 30 & 33, O5)**: Replaced 100ms sleep polling loop in `SyncDaemon::spawn_watcher_coordinator` (`src/daemon.rs`) with channel timeout receiver `signal_rx.recv_timeout(retry_interval)`. Introduced `WatcherSignal::Shutdown` and wired `watcher_signal_tx` into `perform_shutdown`, achieving <50ms shutdown responsiveness and replacing string interpolations with structured key-value bindings.
>   - **Test Support & Verification Double**: Implemented poison-safe `MockSyncStatusObserver` in `src/sync/mock.rs` and re-exported in `src/sync/mod.rs`.
>   - **Quality Verification Gate**: 358 passing tests across workspace (347 all-targets + 11 doc-tests), zero clippy warnings under `-D warnings`, and 100% clean formatting.
> * **New Constraints:**
>   - `io::ErrorKind::NotFound` during worker sync must verify `!source_dir.join(&path).exists()` before evicting immediately without scheduling retries.
>   - Generic I/O errors must be capped at 10 retry attempts with exponential backoff before permanent eviction and observer notification.
>   - Catch-up full scans must throttle repeated failures via `SyncWorkerState.record_catchup_scan_failure` using exponential backoff.
>   - Watcher coordinator thread must be event-driven via `WatcherSignal` and `recv_timeout` rather than active sleep polling.
> * **Pruned:**
>   - 100ms idle sleep polling loop in `spawn_watcher_coordinator` eliminated.
>   - Infinite retry loops on transient missing files and damaged blocks eliminated.
>   - Tight 50ms CPU spin-loop on catch-up scan failure eliminated.

---

> 📝 **Context Update (2026-09-11):**
> * **Feature:** Block 3: Engine Casing, Security & Performance (`/build`, `/audit`)
> * **Changes:**
>   - **On-Disk & DB Casing Alignment (Finding 5, O1)**: Implemented `align_dest_file_casing_if_needed` to inspect on-disk directory entries for casing divergence and perform atomic two-step renames on Windows (`*.syncdir_casetmp_*`). In `sync_file_to_dest_core`, updated SQLite record with `relative_path = excluded.relative_path` and preserved existing 1MB block signatures from `get_block_hashes`, eliminating unnecessary re-hashing. Delimiter normalization in `RelativePath` avoids false casing mismatch triggers on Windows backslashes.
>   - **Symmetrical ReparseCache Eviction (Finding 7 / CWE-59, O2)**: In `delete_file_from_dest`, added symmetrical eviction for `source_path.parent()` alongside `dest_path.parent()`, preventing reparse junction traversal bypasses following file deletion.
>   - **Batch Statement Hoisting & Hash Pre-Allocation (Findings 9 & 19, O3)**: Hoisted prepared cached statements in `SqliteHashStore::save_files_batch` outside the 500-record batch loop and explicitly dropped them prior to `tx.commit()`. Pre-allocated `Vec::with_capacity(64)` in `get_block_hashes`, preventing incremental vector re-allocations on multi-block files.
>   - **Zero-Allocation Full Scan Lookup (Finding 10, O4)**: Refactored `FullScanCoordinator::build_cache_lookup` to return `HashMap<NormalizedCaseFoldedPath<'b>, &'b FileRecord>` referencing borrowed paths from file records, eliminating 50,000+ heap `PathBuf` allocations during startup scans.
>   - **Scanner Path Allocation & Zero-Syscall Junction Checks (Findings 11 & 16, O5)**: Allocated `let path = entry.path()` once per entry in `DirectoryScanner::scan_dir_cancellable`, checking `is_reparse_or_symlink(&entry, &path)` across both directories and files. On Windows, `is_reparse_or_symlink` inspects cached `entry.metadata()?.file_attributes() & 0x400` first, eliminating extra syscalls while reliably detecting junctions.
>   - **Zero-Allocation Path Safety Traversal (Finding 17, O5)**: Replaced `rel_path.components().collect::<Vec<_>>()` with a zero-allocation `peekable()` iterator traversal in `verify_destination_not_reparse_cached` across Windows and Unix. Pruned redundant pre-creation reparse check in `archive_dest_file_only`.
>   - **Fast Nonce, Lazy Stack Buffer & Stream Verification (Finding 18, O5)**: Added `splitmix64` bit mixer (<2ns vs ~100ns Blake3 hashing) for staging nonces. Avoided 64KB stack buffer zeroing when worker scratch buffer is provided via `copy_stream_and_hash`, and verified streamed writes against the computed hash.
>   - **Pruned Redundant Metadata Check (Finding 35, O5)**: Removed duplicate `is_metadata_up_to_date` call in `sync_file_to_dest_core`, consolidating evaluation in a single branch.
>   - **Hardened Watcher Rename Pairs (O6)**: In `DirectoryWatcher::handle_rename_pair`, parsed paths as `Option<RelativePath>` to preserve cross-boundary move events and handled case-only renames via `eq_ignore_ascii_case`.
>   - **Quality Verification Gate (O7)**: 356 passing tests across all targets, zero clippy warnings under `-D warnings`, and 100% clean formatting.
> * **New Constraints:**
>   - On-disk destination casing divergence must be resolved via atomic two-step rename (`align_dest_file_casing_if_needed`).
>   - Casing updates in SQLite must preserve existing block hashes without triggering full file re-reads.
>   - Deleting files from destination must symmetrically evict both destination and source parent paths from `ReparseCache`.
>   - Temporary file staging nonces must use `splitmix64` bit mixing instead of cryptographic hashing.
>   - Directory traversals must inspect reparse attributes on cached `DirEntry.metadata()` before issuing filesystem syscalls.
> * **Pruned:**
>   - Monolithic 50,000-entry `PathBuf` heap allocations during full scan cache lookups eliminated.
>   - Per-batch SQLite statement recompilation within 500-record batch loops eliminated.
>   - Redundant duplicate metadata check in `sync_file_to_dest_core` eliminated.
>   - Redundant pre-creation reparse check in `archive_dest_file_only` eliminated.
>   - 64KB stack zeroing per small file sync when scratch buffer is available eliminated.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Block 4: API Safety, Trait Segregation & Hardening (`/build`, `/audit`)
> * **Changes:**
>   - **Trait Segregation & Interface Segregation Principle (Finding 15, O1)**: Segregated monolithic `SyncEngine` into 5 discrete role traits in `src/sync/engine.rs`: `FileSynchronizer`, `FileDeleter`, `BatchFlusher`, `ScanEngine`, and `ArchiveEngine`. Defined composite `SyncEngine: FileSynchronizer + FileDeleter + BatchFlusher + ScanEngine + ArchiveEngine { fn invalidate_verified_dirs(&self) {} }`. Implemented traits on `LocalSyncEngine<S>` and `MockSyncEngine` with inherent delegation methods, resolving `E0034` trait resolution ambiguity.
>   - **Public API Re-Exports & Type Leakage Elimination (Finding 12, O1)**: Re-exported `ReparseCache`, `FileSyncTaskBuilder`, and all 5 role traits from `syncdir::sync`, eliminating private type leakage in `LocalSyncEngine::reparse_cache`.
>   - **Type-Safe `RelativePath` & `HashStore::list_files` (Findings 21 & 22, O2)**: Upgraded `HashStore::list_files(&self) -> Result<Vec<RelativePath>, SyncError>` across `SqliteHashStore`, `MockHashStore`, `Arc<S>`, and `&S`. Implemented bidirectional `PartialEq<PathBuf>` and `PartialEq<Path>` on `RelativePath`. Preserved `RelativePath::new` as an undeprecated inline wrapper around `try_new`.
>   - **`FileSyncTaskBuilder` & Parameter Transposition Defense (Finding 13, O3)**: Strongly typed `FileSyncTask.rel_path` as `&'a RelativePath` and introduced fluent `FileSyncTaskBuilder` enforcing mandatory source, destination, and staging paths. Migrated all 19 call sites across `types.rs`, `engine.rs`, `delta.rs`, and `small_file.rs`.
>   - **`TargetDir` Deserialization Proxy & Dead Code Elimination (Findings 14 & 28, O4)**: Implemented transparent `RawTargetDir` Serde proxy with `#[serde(try_from = "RawTargetDir", into = "PathBuf")]` in `src/config/target.rs` to validate paths during TOML/JSON deserialization without breaking `From<PathBuf>` or causing `E0119` coherence collisions. Pruned dead Unix validation branch.
>   - **Decoupled `FullScanCoordinator` & Deletion Abort Guard (Findings 8 & 26, O5)**: Abstracted full scan filesystem/database dependencies behind the `FullScanDriver` trait. Implemented pure in-memory `MockFullScanDriver` for testing. Guarded deletion reconciliation with `if scan_complete` to prevent accidental deletion of unscanned destination files upon error or cancellation.
>   - **Hardened Error Attributes & Method Shadowing Resolution (Findings 23, 25 & 34, O6)**: Annotated all error constructors and classifiers in `src/error.rs` with `#[must_use]` and added `is_not_found()` predicate. Renamed shadowed 0-arg `LocalSyncEngine::run_full_scan` to `run_configured_full_scan` with `#[deprecated]` attribute. Simplified `LocalSyncEngine::new(db: S, config: TargetSyncConfig)`.
>   - **Windows Explorer Argument Tokenization & Dialog Error Handling (Findings 31 & 32, O7)**: Implemented `format_explorer_args` returning a single `/select,<path>` `OsString` token for files, avoiding default folder fallback and executable launching. Added structured `tracing::error!` logging on background thread spawn errors in `show_about_dialog` and `show_error_dialog`.
>   - **Fluent `SyncDaemonBuilder` & Executable Doc-Tests (Findings 24 & 36, O8)**: Implemented `SyncDaemonBuilder`, deprecated 6-arg `SyncDaemon::start_with_all_services`, and added runnable `# Examples` doc-tests in `src/lib.rs`.
>   - **Quality Verification Gate**: 381 passing tests across workspace (369 all-targets + 12 doc-tests), zero clippy warnings under `-D warnings`, and 100% clean formatting.
> * **New Constraints:**
>   - Consumers requiring a subset of sync functionality should depend on segregated role traits (`FileSynchronizer`, `FileDeleter`, etc.) rather than the full `SyncEngine`.
>   - `FileSyncTask` must be constructed using `FileSyncTaskBuilder` with typed `&RelativePath`.
>   - `FullScanCoordinator` deletion reconciliation must be guarded by `scan_complete`.
>   - Error classifiers and constructor functions must retain `#[must_use]`.
>   - Windows Explorer file selections must be formatted as single `/select,<path>` tokens.
>   - Daemon construction should use `SyncDaemonBuilder`.
> * **Pruned:**
>   - Monolithic `SyncEngine` coupling eliminated via trait segregation.
>   - 4-`&Path` argument ordering hazard on `FileSyncTask` eliminated.
>   - Dead Unix path validation branch in `TargetDir::validate` eliminated.
>   - Shadowed 0-arg `LocalSyncEngine::run_full_scan` replaced.
>   - 6-arg `SyncDaemon::start_with_all_services` deprecated.
>   - Raw path strings in `HashStore::list_files` replaced with strongly-typed `RelativePath`.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Architecture & Specification Synchronization (Post-Review Hardening)
> * **Changes:**
>   - **`architecture.md § 5` Module Boundaries Synchronized**: Documented 5 segregated role traits (`FileSynchronizer`, `FileDeleter`, `BatchFlusher`, `ScanEngine`, `ArchiveEngine`), composite `SyncEngine` supertrait, `FullScanDriver` decoupling interface, `MockFullScanDriver`, `RawTargetDir` Serde proxy for invariant validation during deserialization, `RelativePath` bidirectional equality (`PartialEq<PathBuf>`, `PartialEq<Path>`), strongly-typed `HashStore::list_files(&self) -> Result<Vec<RelativePath>, SyncError>`, and `SyncDaemonBuilder` primary constructor with event-driven watcher coordinator shutdown synchronization (`signal_rx.recv_timeout`).
>   - **`architecture.md § 8` & `§ 9` Error Handling & Observability Synchronized**: Documented `#[must_use]` annotation on all error classifier predicates and constructors, `SyncError::is_not_found()` transparent I/O error classifier, `tracing = { version = "0.1", features = ["attributes"] }`, `#[tracing::instrument]` attribute instrumentation across subsystem boundaries, thread-scoped execution spans (`sync_worker`, `watcher_coordinator`, `tray_event_loop`), and CWE-117 CRLF log injection defense.
>   - **`architecture.md § 10` Testing Strategy Synchronized**: Updated test suite metrics to 381 passing automated tests (321 lib + 7 bin + 13 integration + 8 property + 20 snapshot + 12 doc-tests, and 1 ignored). Expanded Comprehensive In-Memory Mocks list to 6 test doubles with `MockFullScanDriver`.
>   - **`architecture.md § 14` Technical Debt Synchronized**: Recorded complete resolution of all 36 findings across Blocks 1–4 from the multi-lens code review (`review_report.md`).
>   - **`spec.md` Behavioral Contracts Synchronized**: Recorded verification hash `94283f7`, updated API tables, added 7 behavioral scenarios covering immediate eviction of transient NotFound deletions, 10-retry generic I/O bounds, reachability synchronization on PartialFailure, catch-up scan failure backoff, case-only rename alignment, symmetrical ReparseCache eviction (CWE-59), and full scan deletion reconciliation abort guard.
>   - **Intra-Doc Link Resolution**: Fixed broken rustdoc link in `src/sync/engine.rs:472` (`[run_configured_full_scan]` -> `[Self::run_configured_full_scan]`).
>   - **Quality Verification Gate**: All 381 automated tests, 12 doc-tests, `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo doc --no-deps` passing with exit 0.
> * **New Constraints:**
>   - Architectural specification is strictly 1:1 aligned with production implementation and behavioral contracts in `spec.md`.
> * **Pruned:**
>   - All 7 architectural drift areas from `architecture_recommendations_report.md` resolved.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Block 2 — Storage Subsystem Decoupling (`src/db/`)
> * **Changes:**
>   - **Decomposed `src/db.rs` Monolith (Finding 4)**: Decomposed monolithic `src/db.rs` (54.5 KB, 1,563 LOC) into a structured 4-file submodule: `src/db/traits.rs` (pure storage traits/records, zero `rusqlite` dependencies), `src/db/mock.rs` (in-memory test double), `src/db/sqlite.rs` (concrete SQLite engine), and `src/db/mod.rs` (facade with explicit non-wildcard re-exports). Deleted legacy `src/db.rs`.
>   - **Zero-Allocation Record Ingestion (Finding 15)**: Shifted `HashStore::list_all_records` signature from `Result<HashMap<PathBuf, FileRecord>, SyncError>` to flat `Result<Vec<FileRecord>, SyncError>`. Updated `FullScanDriver`, `LocalSyncEngine`, and `FullScanCoordinator` (`load_cached_records`, `build_cache_lookup`, and `reconcile_deletions`) to consume borrowed slice lookups, eliminating intermediate map allocations during full directory scans.
>   - **Atomic Single-Transaction File Deletion (Finding 17)**: Refactored `SqliteHashStore::delete_file` to delegate directly to `self.delete_files_batch(&[path])`, ensuring all single-file deletions and cascades execute inside an atomic SQLite transaction with complete rollback on failure.
>   - **Exact Unicode Path Key Preservation (Finding 27)**: Removed `normalize_superscripts_cow` from `path_to_sqlite_key` to preserve exact UTF-8 filename bytes on disk (e.g., `doc¹.txt` $\ne$ `doc1.txt`), preventing signature cache misses and key collisions.
>   - **Test Expansion & TDD Verification**: Added 5 new TDD unit test suites (unicode fidelity, atomic rollback via aborting trigger, mixed batch deletions, and flat vector record enumeration). Expanded test suite from 381 to 386 tests (326 lib, 7 bin, 13 integration, 8 property, 20 snapshot, 12 doc-tests).
>   - **Quality Gate**: 100% `rustfmt`, zero clippy warnings (`-D warnings`), zero AST-grep violations, and zero circular imports.
> * **New Constraints:**
>   - `src/db/traits.rs` is a pure leaf domain layer with zero `rusqlite` dependencies.
>   - All file deletions in `SqliteHashStore` must execute within explicit transaction boundaries.
>   - `FileRecord` field encapsulation must be respected across submodules via public accessors (`relative_path()`, `file_size()`, `last_modified()`, `id()`).

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Block 3 — Configuration & Path Domain Layer (`src/config/`, `src/path_util.rs`, `src/db/`)
> * **Changes:**
>   - **`TargetDir` Fallible Conversions & Coherence Resolution (Finding 20)**: Implemented `TryFrom<PathBuf>`, `TryFrom<&Path>`, and `TryFrom<&str>` on `TargetDir` with syntax validation (drive roots `R:\`, UNC prefixes `\\`, forward slash normalization); removed unvalidated `From` implementations to prevent Rust trait coherence collision `E0119` with Serde transparent proxy; migrated all internal test call sites to `TargetDir::from_validated` or `TargetDir::try_from(...).unwrap()`.
>   - **Submodule Encapsulation (Finding 22)**: Retracted submodules `target` and `builder` in `src/config/mod.rs` from `pub mod` to `pub(crate) mod`. Exported domain types exclusively through the `syncdir::config` facade.
>   - **Configuration Decoupling from Storage Subsystem (Finding 23)**: Removed `TryFrom<&Config> for StoreConfig` and `TryFrom<&TargetSyncConfig> for StoreConfig` bridges from `src/config/mod.rs`. Replaced coupling tests in `src/config/tests.rs` with `test_config_block_parameters_for_storage`. Confirmed 0 occurrences of `crate::db` in `src/config/`.
>   - **Builder & Domain Query Ergonomics (Finding 34)**: Applied struct-level `#[must_use = "..."]` to `ConfigBuilder` and `TargetSyncConfigBuilder`. Applied `#[must_use]` to all pure query getters on `Config` and `TargetSyncConfig`, `DestinationCollection` queries (`len`, `is_empty`, `as_slice`), and `RelativePath` queries (`as_path`, `as_forward_slash_str`, `to_storage_key`, `to_ascii_lowercase`). Scoped attributes to prevent clippy `double_must_use` warnings.
>   - **Zero-Allocation `RelativePath` & Storage Decoupling (Finding 35)**: Implemented zero-allocation `RelativePath::as_forward_slash_str(&self) -> &str` and storage-agnostic owned string key `to_storage_key(&self) -> String`. Deprecated `to_sqlite_key` with backward-compatible forward shim. Migrated cache lookup and binding callers in `src/db/sqlite.rs` and `src/db/mock.rs`.
>   - **Test Suite Expansion & Quality Verification Gate**: Expanded automated test suite from 386 to 392 tests (332 lib unit, 7 bin unit, 13 integration, 8 property, 20 snapshot, 12 doc-tests) with 0 failures. 100% `rustfmt`, zero clippy warnings under `-D warnings`, zero AST-grep violations, and zero circular imports.
> * **New Constraints:**
>   - `src/config/` MUST remain completely free of storage dependencies (`crate::db`).
>   - `TargetDir` conversions MUST remain fallible (`TryFrom`) to enforce path syntax validation at boundaries; unvalidated `From` implementations remain prohibited.
>   - SQLite query parameter binding MUST use borrowed `RelativePath::as_forward_slash_str()` to avoid unnecessary string allocations.
>   - Builders (`ConfigBuilder`, `TargetSyncConfigBuilder`) MUST retain struct-level `#[must_use]`.
> * **Pruned:**
>   - Bidirectional coupling between `config` and `db` eliminated.
>   - Redundant heap allocations in `to_sqlite_key()` query binding paths eliminated.
>   - Unvalidated path ingestion via `From` on `TargetDir` eliminated.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Block 5 — Core Sync Engine Decoupling, Fast-Path Casing & Cache Invalidation (`src/sync/`)
> * **Changes:**
>   - **Decomposed `src/sync/engine.rs` Monolith (Finding 3)**: Reduced `src/sync/engine.rs` from 2,522 lines to 720 lines (strictly complying with the <800 LOC ceiling) by extracting pure role traits to `src/sync/traits.rs` (160+ LOC) and unit tests to `src/sync/engine_tests.rs` (1,600+ LOC).
>   - **Hot-Path Fast-Path Casing Alignment Bypass (Finding 1)**: In `LocalSyncEngine::sync_file_to_dest_core`, bypassed expensive directory `read_dir` traversals in `align_dest_file_casing_if_needed` when local database record metadata matches the destination file (`is_verified_cache_hit`). Instrumenting `CASING_ALIGN_READ_DIR_COUNT` test spy verifies zero directory traversals occur on cache hits.
>   - **Path Safety & Cache Hardening (Findings 10, 11, 12, 18)**:
>     - In `src/sync/path_safety.rs`, prevented caching non-existent root paths in `verify_source_not_reparse_cached` and `verify_destination_not_reparse_cached`, ensuring dynamically created roots undergo live verification.
>     - In `ReparseCache::evict_dir`, added read-lock probe before acquiring the write lock to minimize lock contention.
>     - In `src/sync/archive.rs`, preceded `fs::create_dir_all` with `verify_destination_not_reparse`, and guarded both pruning loops (age retention and byte quota) with `fs::symlink_metadata` checks immediately before `fs::remove_file` to eliminate TOCTOU junction substitution windows.
>   - **Scanner & Delta Transfer Efficiency (Findings 16, 32)**:
>     - In `src/sync/scanner.rs`, checked `entry.file_type()?` before allocating `entry.path()`, avoiding unnecessary heap allocations for non-file/non-dir entries.
>     - In `src/sync/delta.rs`, preallocated `new_hashes` using `Vec::with_capacity(expected_blocks)`, added `last_modified_block_count: AtomicUsize`, and skipped collecting modified block indices when verification mode is `MetadataAndFlush`.
>   - **Type Encapsulation & Diagnostics Alignment (Findings 13, 19, 21, 36)**:
>     - Encapsulated `FileSyncTask` with public getters (`relative_path`, `source_dir`, `destination_dirs`, `action`) and `pub(crate)` fields; mapped task builder validation to `SyncError::validation_invariant`.
>     - Unified `FileMetadataSnapshot.size` to `u64` and relocated `is_metadata_up_to_date_raw` to `src/sync/types.rs`.
>     - In `src/sync/full_scan.rs`, tracked `stats.network_offline` explicitly on `e.is_network_offline()`, preventing generic local I/O errors from being misclassified as destination unreachable.
>   - **Test Suite Expansion & Quality Verification Gate**:
>     - Added 7 new unit tests across `path_safety.rs`, `archive.rs`, `delta.rs`, `full_scan.rs`, `types.rs`, and `engine_tests.rs`.
>     - Expanded test suite from 401 to 408 automated tests passing with zero failures (348 lib, 7 bin, 13 integration, 8 property, 20 snapshot, 9 doc-tests).
>     - 100% `rustfmt`, zero clippy warnings under `-D warnings`, zero AST-grep violations.
> * **New Constraints:**
>   - `src/sync/engine.rs` must maintain a strict < 800 line ceiling (currently 720 LOC).
>   - Casing alignment `align_dest_file_casing_if_needed` must be bypassed on verified cache hits.
>   - Non-existent directories must never be cached in `ReparseCache`.
>   - All archive pruning deletions must be guarded by TOCTOU symlink checks immediately before removal.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Block 6 — System Tray UI, Application Lifecycle & Quality Hardening (`src/tray/`, `src/error.rs`, `src/test_support.rs`, `src/sync/`)
> * **Changes:**
>   - **Decomposed Monolithic `src/tray.rs` (Findings 7, 25)**:
>     - Decomposed `src/tray.rs` (1,173 LOC) into modular submodules under `src/tray/`:
>       - `src/tray/mod.rs` (16 LOC): Pure subsystem facade re-exporting public and internal components.
>       - `src/tray/state.rs` (279 LOC): Pure UI state container (`TrayState`, `DestinationState`, `EngineStatus`, `TrayExitReason`) with encapsulated fields and builder patterns.
>       - `src/tray/state_tests.rs` (179 LOC): 15 isolated unit tests for `TrayState` transitions and repaint gating invariants.
>       - `src/tray/dialog.rs` (244 LOC): Native Win32 modal dialogs (`show_about_dialog`, `show_error_dialog`), wide string null-terminated encoder (`to_wide_null_terminated`), `open_path` (reusing `path_util::system_root`), and hardened Explorer argument quoting (`format_explorer_args`).
>       - `src/tray/menu.rs` (193 LOC): `TrayActionHandler` trait, `TrayMenuIds`, and pure context menu construction `build_tray_menu`.
>       - `src/tray/event_loop.rs` (397 LOC): `TrayController`, `TrayEventLoop`, `WinitStatusObserver`, and `run_tray` event pump.
>       - `src/tray/event_loop_tests.rs` (25 LOC): Event loop data structures and event debugging tests.
>       - `src/tray/assets.rs` (197 LOC): Compile-time 32×32 RGBA icon generation and caching.
>     - Every single file under `src/tray/` strictly satisfies the project's <400 LOC design guideline (well below the <800 LOC ceiling).
>   - **Windows Explorer Argument Quoting (Finding 28)**:
>     - Hardened `format_explorer_args` to format `/select,"<path>"` when file paths contain spaces, paired with Win32 `raw_arg` in `open_path` to prevent command shell quote stripping or corruption.
>   - **Test Double Documentation Encapsulation (Finding 9)**:
>     - Annotated canonical test doubles (`MockHashStore`, `MockSyncEngine`, `MockSyncStatusObserver`, `MockNetworkResolver`, `MockStartupRegistry`) with `#[doc(hidden)]` and re-exported them under `syncdir::test_support`.
>   - **CWE-117 Log Injection Sanitization (Finding 29)**:
>     - Converted all 10 Display path logging (`%path.display()`) and `#[tracing::instrument]` span attributes to Debug formatting (`?path`, `?dir`, `?source_root`, `?dest_dir`) across `src/sync/scanner.rs` and `src/sync/archive.rs`. Verified via scoped tracing capture tests.
>   - **Watcher Error Forward-Compatibility & NotFound Classification (Finding 37)**:
>     - Decorated `WatcherError` with `#[non_exhaustive]` in `src/error.rs` and extended `SyncError::is_not_found` to recognize `WatcherError::PathNotFound(_)`.
>   - **Idle Broadcaster Loop Verification (Finding 30)**:
>     - Verified `spawn_command_broadcaster` operates sleep-free using blocking `recv()` on `command_rx`.
>   - **Test Suite Expansion & Quality Verification Gate**:
>     - Full automated test suite expanded to 422 passing tests (365 lib unit, 7 bin unit, 13 integration, 8 property, 20 snapshot, 9 doc-tests).
>     - 100% `rustfmt`, zero clippy warnings under `-D warnings`, zero AST-grep violations, and all module boundaries strictly validated.
> * **New Constraints:**
>   - All files under `src/tray/` must remain <400 LOC.
>   - Win32 Explorer `/select,"<path>"` arguments for files with whitespace must be passed via `raw_arg`.
>   - Test doubles must be decorated with `#[doc(hidden)]` and re-exported under `syncdir::test_support`.
>   - Path logging and tracing spans in scanner and archive must use Debug formatting (`?path`) to prevent log injection (CWE-117).
> * **Pruned:**
>   - Monolithic `src/tray.rs` (1,173 LOC) eliminated.
>   - Explorer launch failures for paths with spaces eliminated.
>   - Test mock clutter from public API documentation eliminated.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** All-Blocks Comprehensive Architectural Hardening & Modularity Audit (Blocks 1–6)
> * **Changes:**
>   - Conducted whole-codebase compliance audit verifying resolution of all 37 qualitative review findings across Blocks 1 through 6.
>   - Verified all 4 verification gates: 100% `rustfmt` compliance, 0 `clippy` warnings under `-D warnings`, 0 `ast-grep` findings, and 422 passing automated tests across workspace targets.
>   - Confirmed zero unwrap, expect, or panic in production code.
>   - Confirmed zero stale stubs, dead code, or unaddressed technical debt.
>   - Confirmed strict compliance with LOC ceiling rules (<800 LOC ceiling across all files).
> * **New Constraints:**
>   - Full codebase maintains zero-warning clippy and ast-grep status across all targets and features.
>   - Release preparation ready for version bump to v0.2.0 and branch merge to `main`.
> * **Pruned:**
>   - All 37 qualitative review findings marked 100% resolved and audited.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Documentation & Architectural Specification Synchronization (`/update-doc` & `/architecture`)
> * **Changes:**
>   - **`spec.md` Verification Hash Updated**: Advanced verification commit hash from `2f489f6` to `8e5cd6d`. Confirmed all 5 required sections (`doc-rules.md §4`) and 11 module contracts are 100% synchronized with zero behavioral drift.
>   - **Package Documentation Alignment**: Confirmed `Cargo.toml [package.description]`, `src/lib.rs //!` crate-level overview, and `README.md` are 100% identical.
>   - **`architecture.md § 6` & `§ 13` Dependency Rules Decoupled**: Updated `config` row in Dependency Direction Rules to remove `db (types only)` from *May Import*, placing `db` under *Must NOT Import* (reflecting Block 3 storage decoupling with 0 `crate::db` occurrences). Removed `& db` from the `config` edge in the Mermaid module interaction diagram.
>   - **`architecture.md § 14` Review Remediation Synchronized**: Expanded §14 to document all 6 blocks and 37 resolved qualitative review findings, adding detailed summaries for Block 5 (Core Sync Engine Decoupling, Fast-Path Casing Alignment Bypass & Reparse Cache Hardening) and Block 6 (System Tray UI Modularization into `<400` LOC files, Explorer Space Quoting, CWE-117 Logging Defense).
>   - **`architecture.md § 15` Storage Path Corrected**: Updated SQLite programmatic migration strategy path reference from `src/db.rs` to `src/db/sqlite.rs`.
>   - **Quality Verification Gate**: All 422 automated tests (365 lib + 7 bin + 13 integration + 8 property + 20 snapshot + 9 doc-tests), `cargo doc --no-deps`, `cargo clippy -- -D warnings`, and `cargo fmt --check` passing with exit code 0.
> * **New Constraints:**
>   - `architecture.md` dependency rules and diagrams strictly reflect zero coupling between `config` and `db`.
> * **Pruned:**
>   - Stale `src/db.rs` path reference and residual `config` -> `db` dependency in architectural documentation eliminated.

---

> 📝 **Context Update (2026-09-12):**
> * **Feature:** Block 1 — Windows Resource Compilation & Build Script Hardening (`build.rs` & `tests/build_script_test.rs`)
> * **Changes:**
>   - **Gated Windows Resources & Dependency Isolation**: `build.rs::main` gated behind `#[cfg(not(test))]`. `winres::WindowsResource` isolated behind `#[cfg(all(windows, not(test)))]` with a no-op stub for tests/non-Windows, enabling `tests/build_script_test.rs` to mount `build.rs` without requiring `winres` in `[dev-dependencies]`.
>   - **Dynamic 4-Tier SDK Discovery Precedence**: Replaced ambient registry dependency with a 4-tier discovery pipeline (`WINRES_TOOLKIT_PATH` → `RC_PATH` → `WINDOWS_SDK_PATH` → ambient registry) without hardcoded local machine paths.
>   - **Subprocess Safety & Confinement (CWE-426 / CWE-427)**: Implemented `validate_path_safety` enforcing that all path arguments from environment variables contain no null bytes, control characters, or unescaped quotes.
>   - **Defensive Icon Asset Ingestion (CWE-1284 / CWE-1287)**: Implemented `validate_icon_asset` and `validate_icon_bytes`, performing single-handle metadata inspection, bounded size validation (`0 < len <= 524,288` bytes), and 6-byte ICO magic header validation (`00 00 01 00` with `image_count >= 1`).
>   - **Fail-Closed Release Policy & Remediation Guide**: `handle_resource_error` provides a structured 5-point remediation box on `PROFILE == "release"` and exits with code 1 (`std::process::exit(1)`), preventing silent error swallowing and legacy Windows UAC virtualization (CWE-390 / CWE-250) while strictly complying with `.ast-grep/rules/unwrap-in-production.yml`. Includes `SYNCDIR_ALLOW_MISSING_ICON=1` escape hatch for headless/container CI.
>   - **Invalidation Triggers**: `emit_rebuild_directives` prints `cargo:rerun-if-changed=syncdir.ico` and `cargo:rerun-if-env-changed` for `WINRES_TOOLKIT_PATH`, `RC_PATH`, `WINDOWS_SDK_PATH`, and `SYNCDIR_ALLOW_MISSING_ICON`.
>   - **Isolated Integration Test Suite**: Created `tests/build_script_test.rs` with 21 unit/integration tests using pure DI closures (`Fn(&str) -> Option<String>`, `Fn(&Path) -> bool`) avoiding thread-unsafe process environment mutations.
>   - **Quality Verification Gate**: All 443 automated tests (365 lib + 7 bin + 21 build_script + 13 integration + 8 property + 20 snapshot + 9 doc-tests), `cargo clippy -- -D warnings`, and `cargo fmt --check` passing with exit code 0. Zero AST-grep or Narsil security violations in `build.rs`.
> * **New Constraints:**
>   - Build scripts must not hardcode local developer paths.
>   - Environment variables must be validated via `validate_path_safety` before being passed to external toolkits.
>   - Release builds fail-closed on resource compilation failure unless `SYNCDIR_ALLOW_MISSING_ICON=1` is provided.
> * **Pruned:**
>   - Ambient swallowing of `winres` compilation errors in `build.rs` eliminated.
>   - Silent generation of unmanifested release binaries subject to UAC virtualization eliminated.


