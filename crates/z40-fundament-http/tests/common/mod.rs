//! Log capture for tests: one global test subscriber that writes into the buffer of the capturing
//! thread.
//!
//! A global subscriber, not a thread-local one per test: with thread-local subscribers, tracing caches
//! per callsite whether anyone is interested, and a test thread without a subscriber that hits a span or
//! event first can leave it disabled for a capturing thread (lost log lines under load). A global
//! subscriber is always interested; events of threads that do not capture are discarded.
#![allow(dead_code, clippy::unwrap_used)]

use std::cell::RefCell;
use std::io::Write;
use std::sync::{Arc, Mutex, Once};

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

thread_local! {
    /// The capture of the current thread, if it captures.
    static CURRENT: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Writes into the current thread's capture, or nowhere.
struct ThreadWriter;

impl Write for ThreadWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CURRENT.with(|c| match c.borrow_mut().as_mut() {
            Some(capture) => capture.write(buf),
            None => Ok(buf.len()),
        })
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Ends the capture of the current thread when dropped.
pub struct CaptureGuard;

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| c.borrow_mut().take());
    }
}

/// Captures all events of the current thread (every level, no colours) until the guard is dropped.
pub fn capture_logs() -> (Capture, CaptureGuard) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(|| ThreadWriter)
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
    });
    let capture = Capture::default();
    CURRENT.with(|c| *c.borrow_mut() = Some(capture.clone()));
    (capture, CaptureGuard)
}

#[cfg(feature = "test-util")]
pub mod app;
#[cfg(feature = "bff-mysql")]
pub mod db;
#[cfg(feature = "bff")]
pub mod memory;
