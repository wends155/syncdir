# syncdir

> A lightweight Windows user-session background sync utility with block-level delta synchronization.

## Overview

`syncdir` is a lightweight, low-footprint Windows background utility that mirrors a local source folder to a destination folder (such as a local path or mapped network share) in real-time. It operates within the user login session to inherit appropriate permissions for accessing network drives.

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
```

The compiled binary will be located at `target/release/syncdir.exe`.

## Usage / Quick Start

When started with no arguments, the `syncdir` daemon automatically loads or creates a configuration file at `%APPDATA%\syncdir\config.toml`, initializes the local signature cache database, and starts the system tray loop in the Windows notification area.

### Configuration

`config.toml` structure and defaults:
```toml
# Source and destination directories to sync
source_dir = "C:/Users/WSALIGAN/source_folder"
dest_dir = "Z:/dest_folder"

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

### CLI Options

`syncdir` can also be run with specific command-line arguments:
- `syncdir --help` or `-h`: Prints help and usage details.
- `syncdir --version` or `-v`: Prints current package version.
- `syncdir --register-startup`: Adds `syncdir` to the Windows Startup Registry (HKCU Run key).
- `syncdir --unregister-startup`: Removes `syncdir` from the Windows Startup Registry.
- `syncdir --autostart`: Starts the background sync daemon (invoked automatically by Windows on startup).

## Features / Feature Flags

- **Block-level Delta Synchronization**: Only transfers modified 1MB blocks of files $\ge$ 10MB.
- **Write Verification**: Reads back and hashes blocks immediately after writing to guarantee block integrity.
- **Timestamp Alignment**: Automatically syncs destination file timestamps to match the source file, allowing fast-path comparison.
- **Real-Time Fs Watcher**: Uses Windows directory notification hooks (`notify` crate) with a configurable debounce filter.
- **Automatic Deletion Archiving**: Moves deleted target files to a timestamped folder (`.syncdir_archive`) on the destination share instead of deleting them permanently.
- **Registry Integration**: Directly toggle auto-launch at system startup via the checkable system tray menu.

## API Surface

For integration details, refer to the library crate modules:
- `syncdir::config`: Configuration parsing and validation models.
- `syncdir::db`: Local SQLite signature caching database implementation.
- `syncdir::sync`: Delta synchronization engine block hashing and verification logic.
- `syncdir::monitor`: Filesystem event debouncer and monitoring worker.
- `syncdir::startup`: Platform-specific Startup Registry configuration.
- `syncdir::tray`: Tray-icon menus and event loops.

## Architecture

Refer to [architecture.md](file:///c:/Users/WSALIGAN/code/syncdir/architecture.md) for detailed descriptions of the design patterns, databases, concurrency loops, and toolchains.

## License

This project is licensed under the MIT License.
