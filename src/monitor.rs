use std::sync::mpsc::Sender;
use crate::error::SyncError;
use crate::config::Config;
use crate::sync::SyncCommand;

pub struct DirectoryWatcher;

impl DirectoryWatcher {
    pub fn start(_config: &Config, _tx: Sender<SyncCommand>) -> Result<Self, SyncError> {
        Err(SyncError::Validation("stub watcher".to_string()))
    }
}
