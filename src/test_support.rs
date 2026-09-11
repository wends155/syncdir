//! Shared test support utilities for tracing capture and verification.

use std::sync::{Arc, Mutex};

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
