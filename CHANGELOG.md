# Changelog

All notable changes to the `syncdir` project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.1.12] - 2026-08-02

### Added
- **Windows Mapped Drive Support (`X:\...`)**:
  - `Config::validate()` explicitly validates and logs Windows mapped drive target paths (`X:\...`, `Z:\...`).
  - Added unit test `test_config_validate_mapped_drive` asserting mapped drive configuration validation.

### Improved
- **Path Compatibility Filtering (`normalize_path`)**:
  - Automatically converts forward slashes `/` -> `\` (e.g. `X:/folder/path` -> `X:\folder\path`).
  - Trims surrounding quotes and whitespace (`"Z:\folder\"` -> `Z:\folder`).
  - Repaired single-backslash UNC network prefixes (`\172...` -> `\\172...`).
  - Trims redundant trailing backslashes while preserving root drive paths (`C:\`, `X:\`).
  - Added unit test `test_normalize_path_filtering` covering forward slashes, trailing slashes, quotes, single-backslash UNC, and root drive paths.

---

## [v0.1.11] - 2026-08-02

### Added
- **Developer & Release Automation (`scripts/`)**:
  - Relocated `check-quality.ps1` and `build-release.ps1` to a git-tracked top-level `scripts/` directory.
  - Portable MSVC static CRT release packaging with automated `dumpbin` linkage check, staging to `dist/`, versioned ZIP archive creation (`syncdir-v0.1.11-x86_64-windows.zip`), and SHA256 checksum generation.
- **Ast-Grep Security Rule**: Added `.ast-grep/rules/lint-suppression-audit.yml` to audit all forms of Rust lint suppressions (`#[allow]`, `#[expect]`, `#![allow]`, `#![expect]`) during `sg scan`.

### Improved
- **Win32 SMB Network Error Classification**: Extended `SyncError::is_network_offline()` with Win32 codes `65` (`ERROR_NETWORK_BUSY`), `121` (`ERROR_SEM_TIMEOUT`), and `1326` (`ERROR_LOGON_FAILURE`).
- **Full Scan Network Resilience**: Integrated early exit in `run_full_scan` loop when encountering network/auth errors, emitting a single structured warning and skipping remaining targets to prevent log spam.
- **Tray UI Clean Event Loop Exit**: Wrapped `exit_reason` state in `Rc<Cell<TrayExitReason>>` in `src/tray.rs` to allow clean Reload Config restarts without compiler suppression warnings.

---

## [v0.1.10] - 2026-08-01

### Added
- **Multi-Layered Testing Infrastructure Uplift**:
  - **Snapshot Testing (`insta`)**: Added `tests/snapshot_tests.rs` with 10 regression-guarding snapshot tests covering `Config` debug formatting, validation errors, `SyncError` display output, and `FileRecord` debug structures.
  - **Generative Property-Based Testing (`proptest`)**: Added `tests/property_tests.rs` with 6 property-based test suites validating block boundary division, Config TOML round-tripping, SMB timestamp tolerance, path traversal safety, sync idempotency, and delta sync isolation.
  - **Pure TrayState Container & Tray Unit Tests**: Extracted `pub struct TrayState` in `src/tray.rs` encapsulating pure status calculations and tooltip formatting, with 8 unit tests validating all `EngineStatus` state transitions.
  - **Enhanced Diff Assertions**: Integrated `pretty_assertions` across all unit and integration test modules for colorized diff output on test failures.
- **Global Host Log Context**: Added root `tracing` span `info_span!("syncdir", host = %sys_info.hostname)` in `src/main.rs` to propagate machine hostname across all structured log lines.

### Improved
- **Test Suite Scale**: Expanded automated test suite from 44 to 68 passing tests (43 unit, 8 integration, 10 snapshot, 6 property, 1 doctest).
- **Documentation & Behavioral Specifications**: Synchronized `spec.md` behavioral contracts, verification metadata hash (`204e4b9`), and `architecture.md` (Sections 5, 10, and 12) with the testing uplift and `TrayState` component additions.

---

## [v0.1.9] - 2026-07-30

### Added
- **Single-Instance Process Execution Enforcement**: Added Win32 session-local named mutex (`Local\syncdir_single_instance`) and `SingleInstanceGuard` RAII handle in `src/main.rs`. Secondary process launches output a message to stderr and exit cleanly with code `0` without spawning duplicate tray icons.
- **TrayExitReason Enum**: Returned by `run_tray` in `src/tray.rs` to signal clean event loop exit reasons (`UserExit`, `Restart`) to `main()`.

### Fixed
- **Taskbar Tray Icon Duplication**: Prevented multiple concurrent daemon instances from launching in the same user login session.
- **Ghost Tray Icon on Process Restart**: Refactored "Reload Config" restart handoff so `TrayIcon::Drop` (`Shell_NotifyIconW(NIM_DELETE)`) unregisters the tray icon from Windows Shell before `main()` drops the process mutex and spawns `syncdir.exe`.

### Documentation
- Updated `spec.md` behavioral specifications and `architecture.md` (Sections 2 and 5) with single-instance mutex enforcement and clean restart handoff contracts.

---

## [v0.1.8] - 2026-07-25

### Added
- **UNC Path Normalization & Preprocessing**: Added TOML 4-backslash UNC string escaping (`preprocess_config_toml()`) and defensive single-to-double backslash normalization (`\172...` -> `\\172...`), preventing TOML unescaping pitfalls on network share paths.
- **Strict Destination Format Validation**: Enforced strict path validation in `Config::validate()`, rejecting relative destination paths that do not start with a drive letter (`C:\`) or UNC network prefix (`\\`).
- **File Telemetry & SMB Timestamp Tolerance**: Added structured `tracing::info!` file copy telemetry (`path`, `target`, `size`) and ±2000 ms destination timestamp tolerance fast-path match for Windows SMB network shares.
- **System Environment & Target Diagnostic Telemetry**: Added `SystemDiagnosticInfo::collect()` in `src/startup.rs` to log OS edition, build number, arch, hostname, username, and app version at startup. Added startup target reachability checking (`dest.exists()`) with structured `INFO` (online) or `WARN` (unreachable) logging.
- **Documentation**: Documented Windows UNC path formatting gotchas in `README.md` and updated `architecture.md` (Sections 2, 5, 9, 14) and `spec.md`.

### Fixed
- **Full Scan Resiliency**: `run_full_scan()` now handles individual file sync and deletion I/O errors gracefully with `tracing::warn!` logging and skip-count summaries instead of aborting the scan.
- **Tray Status UI Telemetry Sync**: Synchronized tray status telemetry with full scan write failures (returning `Ok(false)` on 100% write failure to set tray icon to yellow/DestinationOffline).

---

## [v0.1.7] - 2026-07-25

### Added
- **System Tray "Reload Config" Menu**: Added a "Reload Config" option directly below "Open Config" in the system tray context menu. Validates `config.toml` and performs a clean process re-launch (`restart_process`); displays a native Windows error modal (`show_error_dialog` via `MessageBoxW` with `MB_ICONERROR`) if validation fails while keeping the current daemon running.
- **Shared Test Fixture Helper**: Added `Config::test_default()` helper in `src/config.rs`, refactoring test setup across all unit and integration tests.
- **Expanded Test Suite**: Added unit/integration tests for offline `TriggerFullScan` behavior, `Reload Config` validation error handling, and nested directory archive structure preservation (expanded to 37 passing tests).

### Fixed
- **Startup Full Scan Race Condition**: Fixed race condition where `TriggerFullScan` was skipped at startup due to cached atomic status by evaluating live directory presence.

### Improved
- **Architecture & Behavioral Specifications**: Updated `architecture.md` Sections 2, 5, 10, 14 and `spec.md` to document process re-launch mechanisms, test helpers, and `RegistryBackend` static call decoupling debt.

---

## [v0.1.6] - 2026-07-25

### Added
- **System Tray "About" Menu**: Added an "About" option to the system tray context menu that triggers a native Windows modal dialog (`MessageBoxW`) displaying version, description, copyright notice, and repository URL.
- **Package Author Metadata**: Added `authors = ["Wendell Saligan"]` in `Cargo.toml` and exported shared `pub const COPYRIGHT` constant in `src/lib.rs`.

### Improved
- **CLI Help & Version Output**: Enriched `syncdir --help` and `syncdir --version` with copyright notice (`(c) 2026 Wendell Saligan`) and repository link (`https://github.com/wends155/syncdir`).

---

## [v0.1.5] - 2026-07-25

### Added
- **Optional Destination Directory**: `dest_dir` is now optional (`Option<PathBuf>`) in `Config`, enabling configurations that specify destination directories exclusively via `dest_dirs = [...]`.
- **In-Memory Testing Harnesses**: Added `MockHashStore` (`src/db.rs`) and `RegistryBackend` with `MockStartupRegistry` (`src/startup.rs`) for fast, isolated, cross-platform unit testing.

### Improved
- **Documentation & Behavioral Contracts**: Fully synchronized `architecture.md` module boundaries, `spec.md` behavioral specifications, and rustdoc comments across all public traits and structs.

### Fixed
- **Configuration Parsing**: Fixed a `missing field dest_dir` TOML deserialization error when `config.toml` contains only `dest_dirs`.

---

## [v0.1.4] - 2026-07-25

### Added
- **Multi-Destination Synchronization (Broadcaster Engine)**: Broadcaster architecture supporting multiple target paths (`dest_dirs` array in `config.toml`). Spawns dedicated sync workers per target with isolated Blake3-hashed SQLite cache databases (`sigcache_<hash>.db`).
- **Tray Status UI Telemetry**: Aggregated health status painting and tooltips (Healthy/Blue, Source Offline/Red, Destination Offline/Yellow, Both Offline/Gray) with checkable target status menu entries.
- **Static Security Analysis Rules**: Added `ast-grep` (`sg`) security rules (`unwrap-in-production`, `sql-injection`, `path-traversal-leak`, `scattered-env-var`) including path traversal deletion checks (`fs::remove_file`, `fs::remove_dir_all`).
- **Crates.io Publishing Readiness**: Enriched `Cargo.toml` with `repository`, `homepage`, `documentation`, `readme`, `keywords`, and `categories` metadata.

### Improved
- **Directory Presence & Recovery**: Dynamic directory watcher lifecycle binding (dropping handles when source goes offline and respawning when online).
- **TOML Path Parsing**: Automatic preprocessor doubling of Windows single backslashes in path strings.
- **Documentation Alignment**: Synchronized `architecture.md`, `spec.md`, and module-level rustdoc comments.

---

## [v0.1.3] - 2026-07-14

### Added
- **Windows Startup Registry Integration**: Native HKCU Run key registration (`StartupRegistry`) with `--register-startup`, `--unregister-startup`, and `--autostart` flags.
- **System Tray Checkable Startup Toggle**: Dynamic menu checkbox in tray UI linked to registry state.
- **Crash Diagnostic Panic Hook**: Custom panic hook logging detailed panic location and message before clean exit.

---

## [v0.1.2] - 2026-07-14

### Added
- **Rename Event Processing**: Real-time directory monitor handling of `RenameMode` events paired into `FileDeleted` and `FileModified` sync commands.
- **Relative Path Validation**: Input boundary path sanitization (`is_safe_relative_path`).
- **Database Mutex Refactoring**: Centralized SQLite connection locking and `SyncError::LockPoison` error handling.

---

## [v0.1.1] - 2026-07-14

### Added
- Initial public release of `syncdir` daemon on GitHub.
- In-place block-level delta synchronization for files ≥ 10MB using 1MB Blake3 hashed blocks.
- Real-time directory monitoring with 3-second write debouncing.
- Safe deletion archiving to `.syncdir_archive/` subfolder.
- MIT License and release binary distribution.
