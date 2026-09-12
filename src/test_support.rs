//! Shared test support utilities and canonical in-memory test doubles.

use std::sync::{Arc, Mutex};

#[doc(hidden)]
pub use crate::db::{MockHashStore, MockStoreErrorHook};
#[doc(hidden)]
pub use crate::net::MockNetworkResolver;
#[doc(hidden)]
pub use crate::startup::MockStartupRegistry;
#[doc(hidden)]
pub use crate::sync::{MockSyncEngine, MockSyncStatusObserver};

/// In-memory thread-safe buffer capturing tracing subscriber output.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct TracingCaptureBuffer(Arc<Mutex<Vec<u8>>>);

impl TracingCaptureBuffer {
    /// Create a new empty capture buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert captured bytes to a Lossy UTF-8 String with poison resilience.
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(|p| p.into_inner()).clone();
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Return the length of captured bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Check if buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::io::Write for TracingCaptureBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Execute a closure with a scoped tracing subscriber capturing formatted text.
///
/// Sets subscriber maximum level to TRACE with ANSI styling disabled.
#[doc(hidden)]
pub fn with_captured_tracing<F, R>(f: F) -> (R, String)
where
    F: FnOnce() -> R,
{
    let buffer = TracingCaptureBuffer::default();
    let writer_buffer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer_buffer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let result = tracing::subscriber::with_default(subscriber, f);
    (result, buffer.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::traits::HashStore;
    use crate::net::NetworkResolver;
    use crate::startup::RegistryBackend;
    use std::path::Path;

    #[test]
    fn test_mock_hash_store_export_and_initialization() {
        let store = MockHashStore::new();
        let files = store.list_files().expect("mock store list files");
        assert!(files.is_empty());
    }

    #[test]
    fn test_mock_sync_engine_export_and_initialization() {
        let engine = MockSyncEngine::new();
        assert_eq!(engine.synced_calls().len(), 0);
    }

    #[test]
    fn test_mock_network_resolver_export_and_initialization() {
        let resolver = MockNetworkResolver::new();
        assert!(resolver.is_destination_accessible(Path::new(r"\\server\share")));
    }

    #[test]
    fn test_mock_startup_registry_export_and_initialization() {
        let registry = MockStartupRegistry::new(false);
        assert!(!registry.is_registered().expect("registry check"));
    }
}
