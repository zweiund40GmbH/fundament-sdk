//! Log capture for tests: a thread-local test subscriber writing into a shared buffer.
#![allow(dead_code, clippy::unwrap_used)]

use std::io::Write;
use std::sync::{Arc, Mutex};
use tracing::subscriber::DefaultGuard;

/// A `Write` into a shared buffer.
#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Everything written so far.
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Captures all events of the current thread (every level, no colours) until the guard is dropped.
pub fn capture_logs() -> (Capture, DefaultGuard) {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    (capture, tracing::subscriber::set_default(subscriber))
}

#[cfg(feature = "bff")]
pub mod app;
#[cfg(feature = "bff-mysql")]
pub mod db;
#[cfg(feature = "bff")]
pub mod memory;
#[cfg(feature = "bff")]
pub mod oidc;
