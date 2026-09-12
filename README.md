# syncdir

> A lightweight Windows user-session background sync utility with block-level delta synchronization.

## Overview

`syncdir` is a lightweight, low-footprint Windows background utility that mirrors a local source folder to one or more target destination directories (such as local folders or mapped network shares) in real-time. It operates within the user login session to inherit appropriate permissions for accessing network drives.

To minimize network bandwidth and disk IO, `syncdir` uses a signature-based block-level delta synchronization mechanism:
- Files smaller than 10MB are fully overwritten on change.
- Files 10MB or larger are segmented into 1MB blocks. Only blocks whose Blake3 hashes differ from the local metadata cache are written, enabling efficient sync over slower networks or SMB shares.

## Installation

To build `syncdir` from source, ensure you have Rust installed (v1.93.1+ or stable toolchain).

```powershell
# Clone the repository
git clone https://github.com/wends155/syncdir.git
cd syncdir

# Build the release binary
cargo build --release

# Alternatively, run full quality pipeline and package release ZIP:
.\scripts\check-quality.ps1
.\scripts\build-release.ps1
```

The compiled binary will be located at `target/release/syncdir.exe` (or in `dist/` when using `scripts/build-release.ps1`).

## Usage / Quick Start

When started with no arguments, the `syncdir` daemon automatically loads or creates a configuration file at `%APPDATA%\syncdir\config.toml`, initializes the local signature cache database, and starts the system tray loop in the Windows notification area.

### Configuration

`config.toml` structure and defaults:
```toml
# Source directory to sync
source_dir = "C:/Users/WSALIGAN/source_folder"

# Primary destination directory
dest_dir = "Z:/dest_folder"

# Optional additional destination directories for multiple targets
# dest_dirs = [
#     "Y:/backup_folder_1",
#     "X:/backup_folder_2"
# ]

# Real-time change notification debounce duration in seconds
debounce_seconds = 3

# Directory presence check retry/polling interval in seconds (default: 10)
retry_interval_seconds = 10
 
# Whether to propagate file deletions from source to destination
propagate_deletions = true

# Minimum file size to trigger block-level delta sync (default: 10MB)
block_sync_threshold_bytes = 10485760

# Size of segments for delta sync (default: 1MB)
block_size_bytes = 1048576

# Verify written blocks by reading back and comparing Blake3 signatures
verify_writes = true
```

### Windows & Network Share (UNC) Path Formatting Gotchas

When configuring Windows paths in `config.toml`, choose one of the three supported path styles:

| Style | Example in `config.toml` | Notes |
|:---|:---|:---|
| **Forward Slashes (Recommended)** | `source_dir = "C:/Users/WSALIGAN/source"` | 🟢 Cleanest in TOML double-quoted strings. `syncdir` automatically normalizes `/` to `\` at runtime. |
| **Single-Quoted Literal** | `source_dir = 'C:\Users\WSALIGAN\source'` | 🟢 Preserves standard Windows backslashes without escaping. Single quotes tell TOML to treat `\` literally. |
| **Double-Escaped** | `source_dir = "C:\\Users\\WSALIGAN\\source"` | 🟡 Standard double-quoted TOML, requiring double backslashes `\\`. |

1. **UNC Network Paths (`\\server\share` or `\\172.16.0.60\share`)**:
   - In TOML, double-quoted strings (`"..."`) interpret `\` as an escape character. Writing `"\\172.16.0.60\share"` in double quotes causes TOML to unescape `\\` to a single `\`.
   - **Recommended**: Use single-quoted literal strings (`'\\172.16.0.60\share'`) or forward slashes (`"//172.16.0.60/share"`) in `config.toml` to avoid escaping issues entirely.
   - **Automatic Safeguard**: `syncdir` automatically pre-processes configuration files to escape double backslashes in quoted TOML strings and defensively normalizes single-leading-backslash UNC paths (`\172.16...` -> `\\172.16...`) at runtime.

2. **Absolute Path Requirement**:
   - All source and destination paths must be absolute (starting with a drive letter like `C:\`, `Z:\`, `X:\` or a UNC network prefix `\\`). Relative paths (e.g. `backup/folder`) are rejected during startup validation.

### CLI Options

`syncdir` can also be run with specific command-line arguments:
- `syncdir --help` or `-h`: Prints help and usage details.
- `syncdir --version` or `-v`: Prints current package version.
- `syncdir --register-startup`: Adds `syncdir` to the Windows Startup Registry (HKCU Run key).
- `syncdir --unregister-startup`: Removes `syncdir` from the Windows Startup Registry.
- `syncdir --autostart`: Starts the background sync daemon (invoked automatically by Windows on startup).

## Build & Release Automation

### Build Prerequisites & SDK Toolkit Resolution
When compiling `syncdir` on Windows, `build.rs` embeds the application icon (`syncdir.ico`) and PE application manifest (`asInvoker`). This requires the Windows SDK Resource Compiler (`rc.exe`).

The build script evaluates a deterministic 4-tier discovery hierarchy:
1. **Tier 1 (`WINRES_TOOLKIT_PATH`):** Direct folder containing `rc.exe` / `windres.exe`.
2. **Tier 2 (`RC_PATH`):** Path directly to the `rc.exe` binary or its enclosing directory.
3. **Tier 3 (`WINDOWS_SDK_PATH`):** Windows Kits / SDK root directory (probes `bin/x64/rc.exe`).
4. **Tier 4 (Ambient Probing):** Standard Windows Registry (`HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots`) and PATH.

If building in a headless CI environment or a system without the Windows SDK, set:
```sh
set SYNCDIR_ALLOW_MISSING_ICON=1
```
to downgrade release resource compilation aborts to non-fatal warnings.

### Release Script (`scripts/build-release.ps1`)
The release packaging pipeline can be executed directly with optional parameter overrides:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1 [OPTIONS]
```

| Parameter | Type | Description |
|:---|:---|:---|
| `-WinresToolkitPath` | `[string]` | Path to directory containing `rc.exe` / `windres.exe` (Tier 1 override) |
| `-RcPath` | `[string]` | Path directly to `rc.exe` binary or parent folder (Tier 2 override) |
| `-WindowsSdkPath` | `[string]` | Path to Windows Kits / SDK root folder (Tier 3 override) |
| `-AllowMissingIcon` | `[switch]` | Permissive escape hatch downgrading release icon/manifest PE verification to warnings |
| `-SkipQualityGate` | `[switch]` | Skips the pre-build code formatting and clippy linting check |
| `-SkipCrtCheck` | `[switch]` | Skips `dumpbin /dependents` static CRT dependency inspection |
| `-SkipPeVerification` | `[switch]` | Skips post-build PE `.rsrc`, manifest, and icon integrity verification |

## Features / Feature Flags

- **Multiple Destinations**: Broadcasts filesystem change events from a single source folder to multiple independent target directories, running concurrent isolated sync processes.
- **Block-level Delta Synchronization**: Only transfers modified 1MB blocks of files $\ge$ 10MB using contiguous dirty block coalescing.
- **Small File Fast-Path**: Files < 10MB bypass block hashing and synchronize via efficient whole-file copy.
- **Write Verification**: Reads back and hashes blocks immediately after writing to guarantee block integrity.
- **Timestamp Alignment**: Automatically syncs destination file timestamps to match the source file, allowing fast-path comparison.
- **Real-Time Fs Watcher**: Uses Windows directory notification hooks (`notify` crate) with a configurable debounce filter.
- **Automatic Deletion Archiving**: Moves deleted target files to a timestamped folder (`.syncdir_archive`) on the destination share instead of deleting them permanently.
- **Resilient Windows Networking**: Bidirectional UNC $\longleftrightarrow$ mapped drive translation and automatic SMB session authentication.
- **Registry Integration**: Directly toggle auto-launch at system startup via the checkable system tray menu.

## API Surface

For integration details, refer to the library crate modules:
- `syncdir::config`: Configuration parsing, validation models, `TargetDir`, and `ConfigBuilder`.
- `syncdir::daemon`: `SyncDaemon` orchestrator, `SyncDaemonBuilder`, and `DaemonHandle`.
- `syncdir::db`: Local SQLite signature caching database, `HashStore` trait, `FileRecord`, and `StoreConfig`.
- `syncdir::error`: Structured error types and causal error chaining with `SyncError` and `WatcherError`.
- `syncdir::monitor`: Filesystem event debouncer, `FileWatcher` & `WatcherFactory` traits, and `DirectoryWatcher`.
- `syncdir::net`: Win32 UNC and SMB connection resolution (`NetworkResolver`).
- `syncdir::path_util`: Lexical path canonicalization, slash normalization, and `RelativePath` domain newtype.
- `syncdir::startup`: Platform-specific Startup Registry configuration (`StartupRegistry`).
- `syncdir::sync`: Delta synchronization engine, `FullScanCoordinator`, `ReparseCache`, `DirtyBlockRange` batching, and worker routines.
- `syncdir::tray`: Tray-icon menus, tooltip state machine (`TrayState`), `TrayEventLoop`, and default shell app execution (`open_path`).

## Architecture

Refer to [architecture.md](file:///c:/Users/WSALIGAN/code/syncdir/architecture.md) for detailed descriptions of the design patterns, databases, concurrency loops, and toolchains.

## License

This project is licensed under the MIT License.
