//! Raw TOML deserialization/serialization DTO for syncdir configuration.

use crate::config::target::VerificationMode;
use crate::config::validation::DEFAULT_RETRY_INTERVAL_SECONDS;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_retry_interval() -> u64 {
    DEFAULT_RETRY_INTERVAL_SECONDS
}

/// Raw DTO representing unvalidated TOML configuration structure.
#[derive(Serialize, Deserialize)]
pub(crate) struct RawConfig {
    pub(crate) source_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dest_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dest_dirs: Option<Vec<PathBuf>>,
    pub(crate) debounce_seconds: u64,
    pub(crate) propagate_deletions: bool,
    pub(crate) block_sync_threshold_bytes: u64,
    pub(crate) block_size_bytes: u64,
    pub(crate) verify_writes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verification_mode: Option<VerificationMode>,
    #[serde(default = "default_retry_interval")]
    pub(crate) retry_interval_seconds: u64,
}
