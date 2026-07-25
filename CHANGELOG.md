# Changelog

All notable changes to the `syncdir` project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
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
