# Project Architecture: syncdir

This document outlines the architecture, design patterns, and contracts for the Windows user-session background sync utility `syncdir`.

## 1. Project Overview
`syncdir` is a lightweight, low-footprint Windows background utility that mirrors a local source folder to one or more destination directories (local folders or mapped network shares) in real-time. It is written in Rust, runs completely in user-space, and manages its user interface and health status indicators via a Windows system tray icon.

## 2. Project Objectives & Key Features

### Primary Objectives
* **Bandwidth Optimization**: Minimize network traffic by only writing modified blocks of large files over the local network (SMB).
* **Zero Admin Requirements**: Run completely within the standard user's Windows login session without needing administrator privileges.
* **Instant & Reliable Sync**: Provide real-time sync for active folders while fallback scans guarantee eventually consistent file states.
* **Sleek UX**: Run silently in the background with a clean Windows system tray interface and dynamic status reporting.

### Key Features
* **In-Place Block-Level Delta Sync**: Divide files >10MB into 1MB chunks and calculate cryptographic hashes (Blake3). Rewrite only modified blocks directly in the target destination files using random-access writes.
* **Local Signature Caches**: Maintain isolated local SQLite databases of block hashes for each destination. This prevents downloading or reading destination files over network CIFS/SMB shares to verify differences.
* **Real-time File Watching**: Use a central Windows directory notification hook via `ReadDirectoryChangesW` (debounced by 3 seconds) that broadcasts events to independent destination workers.
* **Archive on Deletion**: Move deleted or overwritten destination files to a `.syncdir_archive/` subfolder on the target share with a timestamp prefix to enable easy manual restore.
* **Startup Target Telemetry & Diagnostic Logging**: On launch, `syncdir` queries Windows Registry (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`) for OS version/edition, build number, arch, hostname, username, and app version (`SystemDiagnosticInfo::collect()`). It tests reachability (`dest.exists()`) for each resolved target and logs `INFO` (online) or `WARN` (unreachable).
* **System Tray Menu**: Right-click menu displaying status details for each configured destination, with options to "Open Config", "Reload Config" (validates configuration, returns `TrayExitReason::Restart` to `main()`, unregisters tray icon, and re-launches process), "View Logs", "Sync Now", "Start on System Startup" (registry auto-start toggle), "About" (version & copyright modal), and "Exit".
* **Single-Instance Process Execution**: On boot, `syncdir` acquires a session-local named Win32 mutex (`Local\syncdir_single_instance`) wrapped in a `SingleInstanceGuard` RAII handle. If another instance is already running, the secondary process outputs a message to stderr and exits with code 0 without spawning duplicate tray icons.

### Non-Goals
* Two-way directory synchronization (strictly one-way source -> destination).
* WAN/Cloud optimization (designed purely for high-speed, local network/SMB shares).
* Complex Graphical Restore Interface (restore is handled manually via standard file explorer).

## 3. Language & Runtime
* **Language**: Rust (Edition 2024)
* **Runtime**: Windows 10 and above (User login session)
* **Toolchain**: `stable-x86_64-pc-windows-msvc` (MSVC Linker)

## 4. Project Layout
```
syncdir/
├── Cargo.toml            # Project dependencies and workspace config
├── Cargo.lock            # Cargo lockfile
├── build.rs              # Windows PE executable resource compilation script (winres)
├── syncdir.ico           # 32x32 32bpp Windows application icon asset
├── LICENSE               # Project MIT license
├── README.md             # Project README documentation
├── architecture.md       # Technical design (this file)
├── spec.md               # Behavioral specifications
├── context.md            # Decisions and history
├── .agents/              # TARS rules, workflows, and scripts
├── tests/
│   ├── integration_tests.rs # Integration testing suite (12 scenarios)
│   ├── property_tests.rs    # Proptest generative invariant suites (8 properties)
│   ├── snapshot_tests.rs    # Insta golden snapshot tests (20 snapshots)
│   └── snapshots/           # Insta snapshot golden files
└── src/
    ├── lib.rs            # Crate library root and module declarations
    ├── main.rs           # Daemon entry point, composition root, DaemonTrayHandler, and telemetry
    ├── daemon.rs         # Background daemon lifecycle and worker orchestration
    ├── config/           # Configuration parsing, validation, and domain models
    │   ├── mod.rs        # Central facade, public re-exports, and StoreConfig bridges
    │   ├── builder.rs    # ConfigBuilder and TargetSyncConfigBuilder
    │   ├── raw.rs        # RawConfig Serde DTO bridge
    │   ├── target.rs     # TargetDir, DestinationCollection, TargetRole, VerificationMode
    │   ├── tests.rs      # Subsystem unit tests
    │   └── validation.rs # TOML preprocessing and numeric bound constants
    ├── path_util.rs      # Path canonicalization and normalization leaf
    ├── net.rs            # Win32 network UNC and mapped drive FFI
    ├── db.rs             # SQLite local database cache layer
    ├── error.rs          # Project-wide error definitions
    ├── monitor.rs        # ReadDirectoryChangesW event monitor
    ├── startup.rs        # Platform-specific registry auto-start hook
    ├── sync/             # Block delta sync engine and background workers
    │   ├── mod.rs        # Module facade and public exports
    │   ├── archive.rs    # Deletion archiving and archive retention pruning (ArchiveManager)
    │   ├── delta.rs      # Blake3 block hashing and delta sync logic (DeltaTransferEngine)
    │   ├── engine.rs     # Core SyncEngine trait implementation and file operations (LocalSyncEngine)
    │   ├── full_scan.rs  # Decomposed multi-stage full directory synchronization (FullScanCoordinator)
    │   ├── mock.rs       # In-memory MockSyncEngine for unit and integration testing
    │   ├── path_safety.rs# Reparse point, ReparseCache, and path traversal security checks
    │   ├── scanner.rs    # Recursive directory scanning and change detection (DirectoryScanner)
    │   ├── small_file.rs # Fast path small file copying and verification (SmallFileTransferEngine)
    │   ├── types.rs      # Standalone FileSyncTask and safe timestamp normalization helpers
    │   └── worker.rs     # Worker thread lifecycle, SyncWorkerContextBuilder, debouncing, and retry queues
    ├── tray.rs           # System tray icon event loop, menus, and process execution
    └── tray/
        └── assets.rs     # Compile-time icon RGBA buffer generation, .rdata tables, and icon cache
```

## 5. Module Boundaries

### `config`
* **Owns**: Parsing `config.toml` from `%APPDATA%\syncdir\config.toml`, strongly-typed path domain modeling via `TargetDir` (`TargetDir::try_new` validating drive roots `R:\`, UNC prefixes `\\`, and slash normalization at construction; `TargetDir::new` deprecated; `RawTargetDir` Serde proxy `#[serde(try_from = "RawTargetDir", into = "PathBuf")]` enforcing invariant validation during TOML/JSON deserialization) and `DestinationCollection` (encapsulating destination lists with Windows case-insensitive deduplication while strictly preserving insertion order), deduplicated sync boundary validation via `validate_sync_boundaries` preventing identical and nested source/dest configurations, encapsulated `TargetSyncConfig` with private fields and `TargetSyncConfigBuilder` enforcing construction invariants (non-empty destination collections, path format validation, recursive sync loop containment via `validate_sync_boundaries`, strictly positive debounce and retry intervals, block size caps $\le 64$MB and positivity, and `block_sync_threshold_bytes >= block_size_bytes`), zero-copy `destinations()` slice (with `Config::dest_dirs` deprecated in favor of `Config::destinations()` and `Config::resolved_dest_dirs()`), Serde backward-compatibility bridging via `RawConfig`, quote-aware TOML bracket parsing (`preprocess_config_toml`), strict path format validation (`Config::validate()` and `TargetDir::validate()`), `TargetSyncConfig::from_config` invariant enforcement returning `Result<Self, SyncError>`, `TryFrom<&Config> for StoreConfig` and `TryFrom<&TargetSyncConfig> for StoreConfig` conversions, and runtime settings. Subsystem is organized into an acyclic hierarchy under `src/config/`: `mod.rs`, `builder.rs`, `raw.rs`, `target.rs`, `validation.rs`, and `tests.rs`.
* **Does NOT own**: Network path resolution (delegated to `net`), filesystem synchronization, database access.
* **Trait Interfaces**: None.

### `path_util`
* **Owns**: Crate-private path canonicalization leaf (`normalize_path`), drive root slash repair (`C:` -> `C:\`), single-backslash root normalization (`\` -> `\`), UNC backslash preservation, UNC host/share parsing (`parse_unc_host_and_share`), lexical parent component collapsing (`collapse_components`), pure path hierarchy comparison (`is_same_or_descendant`), Windows `%SystemRoot%` lookup (`system_root`), and strongly-typed `RelativePath` domain newtype enforcing path safety (`is_safe_relative_path`), forward-slash normalization, leading/trailing whitespace rejection, bidirectional equality (`PartialEq<PathBuf>` and `PartialEq<Path>`), and Serde invariant validation at boundary deserialization. Default shell application launcher (`open_path`) is relocated to `src/tray.rs` (retained here as a self-contained deprecation shim without importing `tray`). Pure leaf module with zero internal crate dependencies except `error::SyncError`.
* **Does NOT own**: Filesystem synchronization, configuration parsing, network resolution, or OS shell execution.
* **Trait Interfaces**: None.

### `net`
* **Owns**: Win32 network UNC and mapped drive FFI (`WNetGetConnectionW`, `WNetAddConnection2W`), `find_mapped_drive_for_unc` with `GetLogicalDrives()` bitmask optimization and Unicode prefix boundary detection, `try_resolve_alternate_path` translating between drive letters and UNC paths, stack-allocated `[0u16; 512]` buffer management with clamped slice length, and private internal free functions. External callers consume network resolution through `NetworkResolver`.
* **Does NOT own**: Config parsing, database operations, sync worker logic.
* **Trait Interfaces**:
  * `NetworkResolver`: Trait abstraction for UNC alternate path resolution and authenticated SMB sessions.
* **Mock Availability**: `MockNetworkResolver` (implemented in `src/net.rs`) with configurable mappings and simulated SMB connection failures for unit testing.

### `daemon`
* **Owns**: Background daemon lifecycle (`SyncDaemon`), fluent configuration consolidation via `SyncDaemonBuilder` (retiring telescoping `start_*` constructors, with 6-arg `start_with_all_services` marked `#[deprecated]`), generic worker orchestration via `SyncEngineFactory` (`SqliteEngineFactory`), non-blocking asynchronous startup (deferring network checks to background threads), central directory watcher thread coordination (`spawn_watcher_coordinator`) with dependency injection of `Arc<dyn WatcherFactory>`, command broadcasting (`spawn_command_broadcaster`), target sync loop validation against network shares (`SyncDaemon::validate_target_loops` accepting `&dyn NetworkResolver`), reconnection scan triggering, `DaemonHandle`, event-driven watcher coordinator shutdown synchronization (`signal_rx.recv_timeout(Duration::from_millis(100))` and `WatcherSignal::Shutdown` replacing 100ms idle sleep polling), and panic-resilient RAII shutdown (`perform_shutdown`) extracting thread panic payloads via `join_thread_and_log_panic`. Decoupled from presentation: `daemon` does NOT import `tray` or `startup`.
* **Does NOT own**: Low-level delta sync hashing, schema migrations, tray UI event loop.
* **Trait Interfaces**:
  * `SyncEngineFactory`: Abstract factory interface for engine instantiation per target directory.
  * Dependency on `WatcherFactory` (from `monitor`).

### `db`
* **Owns**: Connection management to isolated local SQLite databases with WAL mode (`PRAGMA journal_mode = WAL`, `PRAGMA synchronous = NORMAL`, `foreign_keys = ON`), composite index `idx_block_hashes_file_block` on `(file_id, block_index)`, unique index `idx_file_metadata_relative_path`, prepared statement caching (`prepare_cached`), schema versioning (`db_version = "4"`), recording and retrieving file metadata (`FileRecord` strictly encapsulated with private fields, `relative_path: RelativePath`, `file_size: u64`, and getter accessors) and fixed 32-byte block digests (`BlockHash = [u8; 32]`), path-keyed block hash lookups, strongly-typed file enumeration via `HashStore::list_files(&self) -> Result<Vec<RelativePath>, SyncError>`, single-query bulk metadata preload (`list_all_records`), single-block UPSERT updates with `RETURNING id` (`save_file`), in-place path key sanitization (`.drain()`), and safe directory deletion via exact prefix matching `substr(relative_path, 1, length(?1) + 1) = ?1 || '/'`. Generates separate cache database files (`sigcache_<hash>.db`) named using the Blake3 hash of the target path to prevent collisions via `SqliteHashStore::cache_db_path`. Decoupled from `config`: `StoreConfig` is a pure configuration value object constructed via `StoreConfig::new(block_size, threshold)` with zero imports of `config.rs`. `HashStore` is implemented for `Arc<S>` and `&S` for zero-copy multi-threaded sharing.
* **Does NOT own**: Calculating block hashes, filesystem read/writes, or configuration parsing.
* **Trait Interfaces**:
  * `HashStore`: Interface for persisting and querying file block signatures (`BlockHash`).
* **Mock Availability**: `MockHashStore` (implemented in `src/db.rs`) for in-memory unit testing.

### `sync`
* **Owns**: Scanning directory trees with symlink and intermediate directory junction skipping (`verify_destination_not_reparse` validating all ancestor components) and recursion depth limits (`scan_dir` skipping `PermissionDenied` folders), path safety validation (`is_safe_relative_path`), comparing source/destination state with ±2000 ms SMB timestamp tolerance (`FileMetadataSnapshot::is_up_to_date` with `is_metadata_up_to_date_raw` deprecated), fast-path metadata bypass before hashing, active destination truncation/corruption repair in `sync_file_to_dest_core` (`dest_size == src_size` and timestamp verification), on-disk destination filename casing alignment (`align_dest_file_casing_if_needed`), symmetrical `ReparseCache` eviction on deletion (CWE-59 defense), 64KB streamed small-file write verification (`verify_small_file_write`), worker scratch buffer reuse in delta sync, TOCTOU file length truncation protection using actual streamed byte counts, delta sync destination existence checking, robust chunked reads (`read_block`), hashing files in 1MB blocks via Blake3 returning `BlockHash` arrays, performing in-place block updates, reusable dirty range buffer memory management (`DirtyBlockRange::reset`), decoupled periodic and post-full-scan archive pruning with root junction safety in `prune_archive` (verifying `archive_dir` itself is not a junction before traversal, enforcing recursion depth $\le 32$), Windows case-insensitive deletion detection in `run_full_scan`, worker sub-components (`DebounceQueue`, `ReachabilityMonitor`, `SyncWorkerState`), thread-safe source presence tracking (`SourceConnectivityTracker`), testable discrete worker state machine (`SyncWorkerRunner<E>` with deterministic `tick(now)` stepping and `handle_command`), dependency injection of `Arc<dyn NetworkResolver>` into `SyncWorkerContext`, structured manual tracing spans (`sync_file`, `full_scan`, `archive_file`), and running background worker loops (`start_sync_worker`) with exponential backoff retries on `WriteVerificationFailed`, generic I/O 10-retry ceiling, and permanent validation error eviction. All fields of `LocalSyncEngine` and `SyncWorkerContext` are strictly encapsulated; `SyncWorkerContextBuilder` enforces invariant validation.
* **Collaborating Components (Decomposed Engine)**:
  * `sync::types`: Standalone leaf module containing `FileSyncTask` and `FileSyncTaskBuilder` (preventing argument transposition hazards on source/destination/staging paths), alongside timestamp normalization helpers (`safe_epoch_duration_millis`, `safe_modified_millis`), completely eliminating inverted circular dependencies from leaf transfer engines (`delta.rs`, `small_file.rs`) to `engine.rs`.
  * `sync::full_scan::FullScanCoordinator`: Standalone decomposed pipeline coordinator for full directory synchronization with zero-allocation lookups on Windows (`NormalizedCaseFoldedPath`), separating scan stages into dedicated, low-complexity methods (`resolve_active_destination`, `collect_source_files`, `load_cached_records`, `synchronize_files`, `reconcile_deletions`, `prune_archive`). Decoupled from `LocalSyncEngine` via the `FullScanDriver` trait.
  * `sync::small_file::SmallFileTransferEngine`: Standalone leaf engine for fast-path atomic small-file streaming, write verification, `splitmix64` nonce generation, lazy stack buffer allocation, and staging.
  * `sync::delta::DeltaTransferEngine<S: HashStore>`: Standalone leaf engine for in-place delta synchronization, chunked file reading, Blake3 block hashing, and dirty range pooling.
  * `sync::archive::ArchiveManager`: Standalone leaf engine for retention-based archive subfolder management, timestamped backups, root junction verification, safe directory pruning, and `ReparseCache` integration in `archive_dest_file_only`.
  * `sync::scanner::DirectoryScanner`: Standalone leaf engine for directory traversal, single path allocation per entry, batch DB record saving, case-insensitive deletion detection, cancellation, and safety threshold checks.
  * `sync::engine::LocalSyncEngine<S: HashStore>`: Central coordinator composing the collaborating leaf transfer engines, implementing `SyncEngine` by coordination. Implements segregated role traits (`FileSynchronizer`, `FileDeleter`, `BatchFlusher`, `ScanEngine`, `ArchiveEngine`) and `FullScanDriver`. All coordination methods (`run_cancellable_full_scan_impl`, `flush_record_batch`, `delete_file_from_dest`, `prune_destination_archive`, `archive_dest_file_only`, `sync_delta_large_file_core`, `sync_small_file_core`, `run_configured_full_scan`) are consolidated directly in `engine.rs`.
  * `sync::path_safety`: Win32 reparse point validation, ancestor junction guards, two-phase non-blocking cache verification, zero-allocation peekable component iterator traversal, and path traversal defenses.
  * `sync::worker`: Discrete worker state machine (`SyncWorkerRunner`), worker lifecycle loop (`start_sync_worker`), debounce priority queues, exponential backoff, catch-up scan failure throttling, reachability tracking, and `SyncWorkerContextBuilder`.
  * `sync::mock`: Thread-safe mock implementation (`MockSyncEngine`, `MockSyncStatusObserver`, and `MockFullScanDriver`) with zero-panic lock acquisitions for unit and integration testing.
* **Encapsulation**: All submodules are encapsulated via `pub(crate) mod`. The crate-level facade `syncdir::sync` exposes `SyncEngine`, `FileSynchronizer`, `FileDeleter`, `BatchFlusher`, `ScanEngine`, `ArchiveEngine`, `FullScanDriver`, `LocalSyncEngine`, `SyncCommand`, `ScanOutcome`, `ConnectivityState`, `WatcherState`, `SyncWorkerContext`, `SyncWorkerContextBuilder`, `start_sync_worker`, `FileSyncTask`, `FileSyncTaskBuilder`, `ReparseCache`, `MockSyncEngine`, `MockSyncStatusObserver`, and `MockFullScanDriver`.
* **Does NOT own**: Watching directories, UI interactions, daemon lifecycle.
* **Trait Interfaces**:
  * Segregated Role Traits:
    * `FileSynchronizer`: Interface for single-file and buffered synchronization (`sync_file`, `sync_file_buffered`, `sync_file_to_dest_buffered`).
    * `FileDeleter`: Interface for destination file deletion and archiving (`delete_file`, `delete_file_from_dest`).
    * `BatchFlusher`: Interface for flushing in-memory staged synchronization batches (`flush_staged_syncs`).
    * `ScanEngine`: Interface for full directory scanning (`run_cancellable_full_scan`).
    * `ArchiveEngine`: Interface for archive retention pruning (`prune_archive`).
  * Composite Supertrait: `SyncEngine: FileSynchronizer + FileDeleter + BatchFlusher + ScanEngine + ArchiveEngine`: Unified sync execution controller featuring default `invalidate_verified_dirs(&self) {}`.
  * `FullScanDriver`: Decoupling interface implemented by `LocalSyncEngine` for full scan execution (`sync_file_core`, `delete_file_core`, `list_cached_files`, `save_records_batch`).
  * `SyncStatusObserver`: Decoupled listener interface for target destination connectivity transitions.
* **Mock Availability**:
  * `MockSyncEngine` (implemented in `src/sync/mock.rs`): In-memory mock implementing `SyncEngine` and all 5 role traits with dynamic sync/delete handlers, failure injection, and thread-safe call recording.
  * `MockSyncStatusObserver` (implemented in `src/sync/mock.rs`): In-memory observer recording connectivity transitions for verification.
  * `MockFullScanDriver` (implemented in `src/sync/mock.rs`): In-memory driver for isolating `FullScanCoordinator` pipeline tests.

### `monitor`
* **Owns**: Starting the central directory watcher thread (`ReadDirectoryChangesW`) wrapped in `#[must_use]` `DirectoryWatcher`, empty relative path filtering, debouncing file events, broadcasting `SyncCommand` events to destination sync workers via crossbeam/std mpsc channels, and automated notify buffer overflow recovery (`ReadDirectoryChangesW` buffer overflow detection in `handle_watcher_result` dispatching `SyncCommand::TriggerFullScan` to prevent permanently dropped filesystem events). Re-exports strongly-typed `WatcherError`. Decoupled from `Config` (ISP fix accepting `impl AsRef<Path>`).
* **Does NOT own**: Config parsing, sync execution (delegates to `SyncEngine` worker threads).
* **Trait Interfaces**:
  * `FileWatcher`: Trait abstraction for file watchers (`is_watching(&self) -> bool`).
  * `WatcherFactory`: Factory abstraction for watcher creation, enabling mock injection during daemon coordination tests.
* **Mock Availability**: `MockWatcherFactory` and `MockFileWatcher` implementations for testing. `RecommendedWatcherFactory` is the production implementation.

### `main`
* **Owns**: Application composition root, CLI argument parsing, single-instance process mutex acquisition (`acquire_single_instance_mutex` / `SingleInstanceGuard`), dual-writer logging setup, system diagnostic telemetry collection (`SystemDiagnosticInfo::collect()`), panic hook registration, process restart handoff (dropping mutex guard before spawning new process), hosting `DaemonTrayHandler` connecting UI callbacks (`TrayActionHandler`) to `DaemonHandle`, `RegistryBackend`, and `NetworkResolver`, and obtaining status observer directly via `tray_loop.status_observer()` with zero dependencies on `winit`.
* **Does NOT own**: Filesystem watching, tray menu construction, or SQLite database operations.

### `tray`
* **Owns**: Creating the system tray icon, registering menu event handlers, executing the windowless message pump, displaying system toast notifications, signaling clean process restart via `TrayExitReason` enum return from `TrayEventLoop::run`, displaying native error modal dialogs (`show_error_dialog`), event dispatching and UI loop abstraction via `TrayController` (`pub(crate)`), encapsulating winit event proxy behind `TrayEventLoop::status_observer() -> Arc<dyn SyncStatusObserver>`, managing `TrayState` (pure state container tracking strongly-typed `ConnectivityState` and `WatcherState` domain enum transitions, online destination counts, scan notices, and tooltip text formatting without Win32/winit UI side-effects), `DestinationState` parameter grouping with encapsulated precomputed `display_label`, state-transition-gated repaint Win32 IPC (suppressing duplicate `Shell_NotifyIconW` calls), guarded config reload background thread execution, qualified `%SystemRoot%\explorer.exe` process execution delegating to `path_util::open_path`, and toggling Windows startup registration via injected `RegistryBackend` trait (`TrayActionHandler`).
* **Submodules**:
  * `tray::assets`: Compile-time 32×32 RGBA icon buffer generation (`const fn generate_status_rgba`), static `.rdata` tables (`STATUS_RGBA`), zero-panic array caching (`ICON_CACHE` via `OnceLock<[Icon; EngineStatus::COUNT]>`), and graceful healthy fallback.

* **Does NOT own**: Filesystem watching or database execution.

### `startup`
* **Owns**: Reading, registering, and unregistering Windows startup registry keys (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).
* **Does NOT own**: Configuration validation, system diagnostic telemetry collection, or UI execution.
* **Trait Interfaces**:
  * `RegistryBackend`: Interface for Windows startup registry operations.
* **Mock Availability**: `MockStartupRegistry` (implemented in `src/startup.rs`) for cross-platform unit testing.

---

## 6. Dependency Direction Rules

| Module | May Import | Must NOT Import |
|--------|-----------|-----------------|
| `main` | `daemon`, `tray`, `config`, `sync`, `startup`, `net`, `path_util`, `error` | `db` (direct), `winit` |
| `daemon` | `config`, `net`, `monitor`, `sync`, `db` (via factory), `path_util`, `error` | `main`, `tray`, `startup` |
| `tray` | `sync`, `config`, `error`, `startup` (trait), `path_util`, `tray::assets` | `db` (direct), `main`, `daemon` |
| `tray::assets` | `EngineStatus` (super), `error`, `tray-icon` | All other modules |
| `monitor` | `sync`, `path_util`, `error` | `config`, `db`, `tray`, `main`, `daemon` |
| `sync` | `db` (trait), `config`, `net`, `path_util`, `error` | `monitor`, `tray`, `main`, `daemon` |
| `db` | `path_util`, `error` | `config`, `sync`, `monitor`, `tray`, `main`, `daemon` |
| `startup` | `error` | `config`, `sync`, `db`, `monitor`, `tray`, `main`, `daemon` |
| `net` | `path_util`, `error` | `config`, `sync`, `db`, `monitor`, `tray`, `main`, `daemon` |
| `config` | `path_util`, `db` (types only), `error` | `net`, `sync`, `monitor`, `tray`, `main`, `daemon` |
| `path_util` | `error` | All other internal modules |
| `error` | None | All |

> **Cycle Resolution Note**: The historical circular dependency between `config` and `net` is permanently resolved by extracting path canonicalization into `path_util`. `db` is completely decoupled from `config`, importing `path_util` (for `RelativePath`) and `error` only. `daemon` is decoupled from `tray` and `startup`, using `path_util::open_path` and delegating UI/startup dispatching to `main`. `main` acts as the composition root, decoupled from `winit`, consuming `Arc<dyn SyncStatusObserver>` from `tray`.

---

## 7. Toolchain
* **Formatter**: `cargo fmt --check`
* **Linter**: `cargo clippy -- -D warnings`
* **Test Runner**: `cargo test`
* **Verification Command**: `cargo fmt --check && cargo clippy -- -D warnings && cargo test`

## 8. Error Handling Strategy
* We use `thiserror` to define a single project-wide `SyncError` enum with `#[non_exhaustive]`.
* Swallowing errors is strictly prohibited. If a sync fails (e.g. network share disconnects), it logs the warning and schedules a retry.
* Error propagation uses the standard `?` operator.
* **Causal Error Chains**: `SyncError` variants (`Db`, `Config`, `LockPoison`, `Watcher`, `Tray`, `Registry`) preserve causal error sources via `(String, #[source] Option<Box<dyn std::error::Error + Send + Sync>>)`. Dual constructors (`name` and `name_with_source`) exist for all multi-parameter variants.
* **Write Verification Failure Differentiation**: `SyncError::WriteVerificationFailed { path: PathBuf }` explicitly distinguishes data integrity verification mismatches from fatal syntax/configuration errors, allowing the worker loop to schedule exponential backoff retries rather than evicting files permanently.
* **Lock Poisoning Generalization**: `SyncError::LockPoison` represents general mutex poisoning across all modules rather than being coupled to database locks.
* **Startup Registry Error Handling**: Operations discriminate `std::io::ErrorKind::NotFound` from unexpected errors (e.g., access denied), preserving typed Win32 error causality.
* **Prevention Linting**: An ast-grep rule (`error-stringification-in-map-err`) enforces that errors inside `.map_err()` closures use `_with_source` constructors rather than lossy stringification.
* **Network Disconnect Classification**: `SyncError::is_network_offline()` inspects `std::io::Error::raw_os_error()` for Win32 SMB disconnect codes (53 `ERROR_BAD_NETPATH`, 59 `ERROR_UNEXP_NET_ERR`, 64 `ERROR_NETNAME_DELETED`, 67 `ERROR_BAD_NET_NAME`).
* **Panic-Free Architecture**: Production code contains zero `.unwrap()` or `.expect()` calls. Worker threads return `Result<JoinHandle<()>, SyncError>` and worker execution loops handle errors gracefully with retry queues.
* **Timestamp Safety**: File modification timestamps are normalized via `safe_modified_millis()` (clamping pre-1970 timestamps to 0 with warning logs) and restored via `safe_epoch_duration_millis()` (preventing wrapping integer underflow on `src_mod as u64`).
* **Domain Subsystem Error Isolation**: Specialized leaf modules define dedicated domain error enums rather than polluting crate-wide error variants. For example, `WatcherError` (`src/error.rs`, re-exported in `src/monitor.rs`) models `notify::Error`, `PathNotFound`, `ChannelDisconnected`, and `Other`, wrapped transparently in `SyncError::Watcher(#[from] WatcherError)` to preserve typed error downcasting and causal inspection.
* **Permanent Validation Failure Classification**: `SyncError::Validation { kind: ValidationKind, message: String }` categorizes errors via the strongly-typed `ValidationKind` enum (`Security`, `ReparsePoint`, `RecursiveLoop`, `Invariant`, `Transient`). `SyncError::is_permanent_validation_failure()` inspects `kind.is_permanent()` using pattern matching rather than fragile substring comparisons. Permanent security and invariant violations (path traversal, reserved DOS device names, reparse point junctions, recursive sync loops) trigger immediate eviction from worker retry queues without retry exhaustion, while transient errors preserve exponential backoff retries.
* **Must-Use Classification & Constructor Safety**: All error classifier predicates (`is_network_offline`, `is_cancelled`, `is_not_found`, `is_permanent_validation_failure`) and constructors (`validation_*`, `io_with_source`, `config_with_source`) in `src/error.rs` are annotated with `#[must_use]`, preventing silent discarding of validation results or error values.
* **I/O Not Found Classifier**: `SyncError::is_not_found()` transparently inspects nested `SyncError::Io` error kinds and underlying `io::ErrorKind::NotFound` across Windows and cross-platform error trees, enabling immediate eviction of deleted source files from debounce retry queues without exponential backoff loops.

## 9. Observability & Logging
* **Framework**: `tracing` with `tracing-subscriber` and `tracing = { version = "0.1", features = ["attributes"] }`.
* **Outputs**:
  * Dev: Stdout
  * Prod: `%APPDATA%\syncdir\logs\syncdir.log.YYYY-MM-DD` (daily log rotation via `tracing-appender`)
* **Log Levels**: `INFO` for file copy telemetry and target reachability, `WARN` for recoverable errors/unreachable targets, `ERROR` for crashes/network loss, `DEBUG` for file block comparisons.
* **Declarative Instrumentation**: `#[tracing::instrument]` attribute instrumentation applied across all subsystem boundaries (`LocalSyncEngine`, `SmallFileTransferEngine`, `DeltaTransferEngine`, `DirectoryScanner`, `ArchiveManager`), capturing inputs with `skip(...)` on heavy data structures.
* **Thread-Scoped Execution Spans**: Background threads enter dedicated contextual spans (`sync_worker`, `watcher_coordinator`, `tray_event_loop`) so all asynchronous log records carry thread and destination context.
* **Log Injection Sanitization (CWE-117 Defense)**: Structured log events use `Debug` formatting (`?rel_path`) or validated domain newtypes to escape carriage returns and line feeds, preventing CRLF log injection in user-controlled filesystem paths.

## 10. Testing Strategy
* **Test Suite Metrics**: 381 total automated tests passing with zero regressions and zero warnings across all targets (321 unit tests in `src/lib.rs`, 7 in `src/main.rs`, 13 integration tests, 8 property tests, 20 snapshot tests, 12 doc-tests, and 1 ignored).
* **Unit Tests**: Co-located `#[cfg(test)]` modules across `src/config.rs` (path normalization, mapped drive resolution, block size/threshold validation, builder invariants), `src/net.rs` (Win32 FFI buffer safety and mapped drive lookups), `src/db.rs` (CRUD, exact prefix cascade deletion, BlockHash signatures), `src/path_util.rs` (lexical parent component collapsing, UNC parsing, hierarchy comparison, system root detection), `src/startup.rs`, `src/tray.rs` (testing `TrayState` status transitions, open_path qualification, and tooltip text formatting), `src/monitor.rs` (watcher buffer overflow recovery and event dispatching), and the decomposed `src/sync/` submodules:
  * `src/sync/engine.rs`: Composed `LocalSyncEngine` end-to-end regression, metadata timestamp tolerances, TOCTOU size protection, directory creation, destination file truncation repair, two-phase lock release, and reparse cache invalidation.
  * `src/sync/delta.rs`: Standalone `DeltaTransferEngine` Blake3 chunk hashing, delta sync dirty block updates, and read-back verification.
  * `src/sync/small_file.rs`: Standalone `SmallFileTransferEngine` fast-path atomic staging, sampled verification, and zero-byte files.
  * `src/sync/scanner.rs`: Standalone `DirectoryScanner` directory traversal recursion limits, permission bypass, and case-insensitive deletion detection.
  * `src/sync/archive.rs`: Standalone `ArchiveManager` retention-based archive subfolder management, timestamped backups, root junction verification, and safe directory pruning.
  * `src/sync/path_safety.rs`: Traversal defense, reserved DOS devices, ADS rejection, ancestor junction caching, and two-phase non-blocking checks.
  * `src/sync/worker.rs`: Discrete `SyncWorkerRunner` stepping, debounce min-heap queue stress testing, exponential backoff, reachability tracking, and permanent validation error eviction.
  * `src/sync/mock.rs`: Recording mock engine verification.
* **Integration Tests**: `tests/integration_tests.rs` (13 tests) simulating standard files, deletions, directory updates, configuration reload validation, rename event pairing, worker reachability offline drain guards, subsecond precision, and `run_tray` interface compilation.
* **Snapshot Tests**: `tests/snapshot_tests.rs` (20 tests) using `insta` (v1) for regression-guarding snapshot assertions on `Config` debug formatting, validation errors (including zero block size/threshold and zero debounce), `SyncError` display output (including `SyncError::WriteVerificationFailed` and `SyncError::Registry`), `TargetSyncConfig`, and `FileRecord` structures.
* **Property-Based Tests**: `tests/property_tests.rs` (8 tests) using `proptest` (v1) for invariant validation (block boundary division, TOML round-tripping, `is_metadata_up_to_date_raw` timestamp delta evaluation across ±10000ms, `DirtyBlockRange` chunk coalescing, path traversal safety wired directly to `is_safe_relative_path`, sync idempotency, and delta sync single-block isolation).
* **Assertions & Structural Diffing**: `pretty_assertions` (v1) for colorized diff output on test failure assertions across all test modules.
* **Shared Test Fixtures**: `Config::test_default()` helper for consistent test configuration across unit and integration tests.
* **Comprehensive In-Memory Mocks**: Six isolated mock implementations providing 100% test isolation:
  * `MockHashStore` (`src/db.rs`): In-memory signature store without SQLite I/O.
  * `MockStartupRegistry` (`src/startup.rs`): In-memory registry backend without HKCU mutation.
  * `MockNetworkResolver` (`src/net.rs`): In-memory drive/UNC translator and SMB failure simulator.
  * `MockWatcherFactory` & `MockFileWatcher` (`src/monitor.rs`): In-memory directory watcher and factory without Win32 OS threads.
  * `MockSyncEngine` & `MockSyncStatusObserver` (`src/sync/mock.rs`): Thread-safe recording sync engine with dynamic handler injection and connectivity observation.
  * `MockFullScanDriver` (`src/sync/mock.rs`): In-memory test double isolating `FullScanCoordinator` pipeline execution without filesystem or SQLite side-effects.

## 11. Documentation Conventions
* Every public struct, trait, and function must be documented using standard triple-slash `///` comments.
* Module-level documentation must be present at the top of each file.

## 12. Dependencies & External Systems
* **Production Dependencies**:
  * `notify` (v6): Cross-platform file monitoring wrapping `ReadDirectoryChangesW` on Windows.
  * `rusqlite`: Connection to local embedded SQLite database.
  * `blake3`: Extremely fast cryptographic hashing.
  * `tray-icon` (v0.14) & `winit` (v0.29): For system tray creation and event loop.
  * `serde` & `toml`: Parsing `config.toml`.
  * `thiserror`: Unified error handling.
  * `winres` (v0.1, Windows target build-dependency): Windows PE binary resource compilation embedding `syncdir.ico` application icon.
* **Development & Testing Dependencies**:
  * `tempfile` (v3): Temporary directory creation for integration tests.
  * `pretty_assertions` (v1): Colorized structural diff assertions.
  * `insta` (v1): Snapshot testing engine.
  * `proptest` (v1): Generative property-based testing framework.
* **Developer & Agent Tooling**:
  * `Narsil MCP`: Code intelligence for blast radius analysis, import graph, symbol discovery, and security scanning.
  * `Sequential Thinking MCP`: Structured multi-step reasoning for planning and audit phases.
  * `Context7 MCP`: Upstream documentation lookup for external crate APIs.
  * `Knowledge-RAG MCP` (`search_knowledge`): Local knowledge index for pre-ingested dependency API docs and TARS rule context. Query-first convention across `/toolcheck`, `/build`, `/plan-making`, `/feature`, `/issue`.

## 13. Architecture Diagrams

### Module Interaction Graph
```mermaid
graph TD
    main --> daemon & tray & config & sync & startup & net & path_util & error
    daemon --> config & net & monitor & sync & path_util & error
    daemon -.->|via factory| db
    tray --> sync & config & path_util & startup & error
    monitor --> sync & path_util & error
    sync --> db & config & net & path_util & error
    db --> path_util & error
    config --> path_util & db & error
    net --> path_util & error
    startup --> error
```

### Data Flow Diagram (Sync Action)
```mermaid
sequenceDiagram
    participant OS as Windows OS (notify)
    participant Mon as DirectoryWatcher Thread
    participant Workers as Sync Worker Threads (per dest)
    participant DB as SQLite Cache (per dest)
    participant FS as Destination Folders
    
    OS->>Mon: File modified event
    Note over Mon: Debounce 3s
    Mon->>Workers: Broadcast SyncCommand::Sync(path)
    loop for each Sync Worker Thread
        Workers->>DB: get_file_signatures(relative_path)
        DB-->>Workers: Block hash signature array
        Note over Workers: Hash file in 1MB blocks (Blake3)
        Note over Workers: Calculate delta block differences
        Workers->>FS: Open target file (read-write)
        loop for each changed block
            Workers->>FS: Seek to offset & overwrite block bytes
        end
        Workers->>FS: Update Last-Modified metadata
        Workers->>DB: update_file_signatures(relative_path, new_hashes)
    end
```

## 14. Known Constraints & Technical Debt
* **Network Latency & Disconnect Resiliency**: If the network connection to a destination mapped share drops, `syncdir` records the failure for that specific target worker, falls back to the resolved UNC path via `NetworkResolver`, skips sync for the file, and retries with exponential backoff or during the next periodic scan when the share becomes reachable.
* **Local DB Location**: Stored in `%APPDATA%\syncdir\sigcache_<hash>.db` (where `<hash>` is the Blake3 hash of the destination directory path). If deleted, it will rebuild automatically during the next full scan by hashing the source directory.
* **UNC Path TOML Escaping Gotcha**: In standard TOML, double-quoted strings (`"\\172.16.0.60\share"`) unescape `\\` to a single backslash (`\172.16...`). `syncdir` works around this by pre-processing TOML strings to convert `\\` to `\\\\` before parsing, defensively auto-correcting single-leading-backslash UNC paths (`\172...` -> `\\172...`), and recommending single-quoted literal strings (`'\\172.16.0.60\share'`) or forward slashes (`"//172.16.0.60/share"`).
* **Startup Registry Decoupling**: Fully resolved via DIP refactoring (`run_tray<H, R>` receives registry backend implementation by value, decoupling UI from static Win32 registry calls).
* **Database Version 4 & Schema Optimization**: With `db_version` bumped to `"4"`, SQLite PRAGMAs enable WAL mode, foreign keys, normal synchronization, composite indexing on `block_hashes(file_id, block_index)`, exact prefix matching `substr(relative_path, 1, length(?1) + 1) = ?1 || '/'`, and `RETURNING id` UPSERTs. Old caches from previous schema versions are automatically invalidated and rebuilt on startup.
* **Two-Phase Reparse Verification & Bounded ReparseCache**: Holding cache locks across remote SMB `symlink_metadata` calls causes lock contention across worker threads. Path safety employs a two-phase check with `ReparseCache`: first probing the cache under an `RwLock` read guard, performing remote filesystem I/O with no lock held, and acquiring a write lock only to insert verified ancestors. `ReparseCache` bounds memory by separating shallow ancestors ($\le 3$ relative depth, up to 50,000 entries) from deep ancestors (up to 10,000 entries with automatic oldest eviction).
* **Reparse Cache Freshness & Dynamic Invalidation**: In-memory verified directory sets (`ReparseCache`) are cleared during full scans (`SyncEngine::invalidate_verified_dirs`) and have affected directory prefixes removed upon file deletion (`LocalSyncEngine::evict_verified_dir`), preventing directory substitution windows after initial validation.
* **Worker Debounce Batch Persistence & Fallback**: Worker loops buffer file sync transfers in-memory (`sync_file_to_dest_staged`) up to 500 files per tick, committing SQLite records in a single batch transaction via `flush_staged_syncs`. If SQLite batch persistence fails, `flush_record_batch` automatically falls back to individual `save_file` calls to ensure partial progress and transactional resilience.
* **Zero-Allocation Superscript Path Normalization**: `normalize_superscripts_cow` in path safety uses an ASCII fast-path returning `Cow::Borrowed` when no unicode superscripts are present, eliminating up to 10 unconditional heap string allocations per path component.
* **Bounded Archive Candidate Discovery**: `prune_archive` enforces a 5,000-candidate scan ceiling (`MAX_ARCHIVE_PRUNE_CANDIDATES`) with early traversal termination, preventing unbounded filesystem walks and memory consumption during archive retention checks.
* **Multi-Lens Code Review Remediation (Blocks 1–4)**: All 36 findings from the multi-lens code review (`review_report.md`) across correctness, performance, security, architecture, and API design are fully resolved:
  * **Block 1: Observability & Tracing Modernization**: Upgraded to `tracing = { version = "0.1", features = ["attributes"] }`, deployed `#[tracing::instrument]` across subsystem boundaries, established thread-scoped worker spans (`sync_worker`, `watcher_coordinator`), sanitized user paths against CRLF log injection (CWE-117), scrubbed usernames/PII in diagnostic logs, and implemented flush-guaranteed crash reporting in the panic hook.
  * **Block 2: Worker Queue & State Resilience**: Hardened `SyncWorkerRunner` discrete tick loop, immediate eviction of transient `NotFound` source deletions, capped generic I/O retry loops at 10 attempts with exponential backoff, throttled catch-up scan failures to avoid tight spin loops, synchronized destination reachability on `ScanOutcome::PartialFailure`, and converted watcher coordinator shutdown to zero-sleep event-driven timeout synchronization (`signal_rx.recv_timeout`).
  * **Block 3: Engine Casing, Security & Performance**: Destination on-disk casing alignment via atomic two-step renames (`align_dest_file_casing_if_needed`), SQLite UPSERT casing updates preserving block signatures, symmetrical `ReparseCache` eviction for both source and destination parent paths (CWE-59 defense), prepared statement caching in `save_files_batch`, block hash vector pre-allocation, zero-allocation full scan cache lookups (`NormalizedCaseFoldedPath`), single-path directory scanner inspection, zero-allocation peekable path safety traversal, and `splitmix64` small-file staging nonces.
  * **Block 4: API Safety, Trait Segregation & Hardening**: Monolithic `SyncEngine` segregated into 5 discrete role traits (`FileSynchronizer`, `FileDeleter`, `BatchFlusher`, `ScanEngine`, `ArchiveEngine`) unified by a composite supertrait; `FullScanCoordinator` decoupled behind `FullScanDriver` with `MockFullScanDriver`; `FileSyncTask` protected by fluent `FileSyncTaskBuilder` with typed `&RelativePath`; `TargetDir` protected by `RawTargetDir` Serde proxy; `RelativePath` bidirectional equality; `SyncError` constructors annotated with `#[must_use]`; and `SyncDaemonBuilder` introduced.

## 15. Data Model

### SQLite Cache Schema

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| **Table: file_metadata** | | | |
| `id` | INTEGER | PK, AUTOINCREMENT | File record ID |
| `relative_path` | TEXT | NOT NULL, UNIQUE | Relative file path from source root |
| `file_size` | INTEGER | NOT NULL | Last known size of the file |
| `last_modified` | INTEGER | NOT NULL | Last modified Unix timestamp |
| | | | |
| **Table: block_hashes** | | | |
| `id` | INTEGER | PK, AUTOINCREMENT | Block record ID |
| `file_id` | INTEGER | FK (file_metadata.id) ON DELETE CASCADE | Reference to file |
| `block_index` | INTEGER | NOT NULL | Zero-indexed chunk position |
| `hash` | BLOB | NOT NULL | Blake3 256-bit hash (32 bytes: `BlockHash`) |

* **Indexes**: `idx_block_hashes_file_block` on `block_hashes (file_id, block_index)` for O(log N) block lookup performance.
* **PRAGMAs**: `journal_mode = WAL`, `synchronous = NORMAL`, `foreign_keys = ON`, `temp_store = MEMORY`.

```mermaid
erDiagram
    file_metadata ||--o{ block_hashes : "has blocks"
    file_metadata {
        integer id PK
        text relative_path
        integer file_size
        integer last_modified
    }
    block_hashes {
        integer id PK
        integer file_id FK
        integer block_index
        blob hash
    }
```

### Migration Strategy
Migrations are managed in `src/db.rs` programmatically. At startup, `db` runs a `CREATE TABLE IF NOT EXISTS` statement for both tables and creates the composite index to guarantee schema availability. Metadata table checks enforce `db_version` compatibility.

## 16. Environment Configuration
No external APIs or environment variables are required. Configuration is loaded entirely from `%APPDATA%\syncdir\config.toml` containing:

```toml
source_dir = "C:/Users/username/Documents"

# Multiple destinations:
dest_dirs = [
    "Z:/Backup",
    "Y:/SecondaryBackup"
]

# Single destination fallback (backward-compatible):
# dest_dir = "Z:/Backup"

debounce_seconds = 3
retry_interval_seconds = 10
propagate_deletions = true
block_sync_threshold_bytes = 10485760 # 10MB
block_size_bytes = 1048576 # 1MB
verify_writes = true # Verify rewritten blocks by hashing read-back bytes
```
