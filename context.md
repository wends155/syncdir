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
  * Tray Menu: Open Config, View Logs, Sync Now, Exit.
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

