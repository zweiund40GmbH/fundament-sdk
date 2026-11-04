//! Reading a change feed (`docs/protocol.md` §2): [`FeedConsumer::run_once`] takes the run lock, fetches
//! pages from the stored cursor until `hasMore` is `false` (at most `max_pages`), applies each page
//! together with its cursor, handles `gap` and records the result of the run.
//!
//! The app connects its own storage through three traits; `replica-mysql` ships implementations for
//! MariaDB/MySQL, and the `replica` feature turns a record store into a [`PageSink`]:
//!
//! * [`RunLock`] – one run per source across all instances
//! * [`CursorStore`] – cursor and health per source
//! * [`PageSink`] – applies a page **and** moves the cursor in one transaction
//!
//! Scheduling stays in the app (its own worker); [`slot_for`] gives a UTC slot name so several workers
//! enqueue a run only once per interval.

mod client;
mod config;
mod run;
mod schedule;

pub use client::FetchError;
pub use config::{DEFAULT_INTERVAL_SECS, FeedConsumerConfig, INTERVAL_RANGE};
pub use run::{
    BuildError, DEFAULT_MAX_PAGES, FeedConsumer, FeedConsumerBuilder, RunOutcome, RunReport,
};
pub use schedule::slot_for;

use crate::health::SourceHealth;
use crate::protocol::feed::FeedEvent;
use chrono::{DateTime, Utc};
use std::future::Future;

/// A failure of the app's storage. Wraps the original error; it is returned to the app, never sent
/// over the wire.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct StoreError(Box<dyn std::error::Error + Send + Sync>);

impl StoreError {
    /// Wraps `error`.
    pub fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

/// Stored position and health of one source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorState {
    /// `nextCursor` of the last applied page (`0` = read from the beginning).
    pub cursor: u64,
    /// Times and result of the last runs.
    pub health: SourceHealth,
}

/// One run per source at a time, across all instances of the app (e.g. `GET_LOCK` on a held database
/// connection).
pub trait RunLock: Send + Sync {
    /// Proof of the held lock; given back to [`RunLock::unlock`]. A guard that is dropped without
    /// `unlock` (the run was cancelled or panicked) must still release the lock, e.g. by closing the
    /// connection that holds it.
    type Guard: Send;

    /// Takes the lock of `source` without waiting; `None` when another run holds it.
    fn try_lock(
        &self,
        source: &str,
    ) -> impl Future<Output = Result<Option<Self::Guard>, StoreError>> + Send;

    /// Releases the lock.
    fn unlock(&self, guard: Self::Guard) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// Cursor and health per source.
pub trait CursorStore: Send + Sync {
    /// The stored state of `source`; `None` before the first run.
    fn load(
        &self,
        source: &str,
    ) -> impl Future<Output = Result<Option<CursorState>, StoreError>> + Send;

    /// Sets the cursor of `source` back to 0 (the feed reported a gap). Stored records stay; their
    /// versions decide what the resync writes.
    fn reset(&self, source: &str) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Records the end of a run at `at`: `error = None` is a success. Implementations keep the
    /// previous result to detect changes ([`crate::health::transition`]).
    fn finish_run(
        &self,
        source: &str,
        error: Option<FetchError>,
        at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// Applies a page. The cursor must move in the **same transaction** as the data, so it never runs
/// ahead of it; applying the same page twice must change nothing (version rule).
pub trait PageSink: Send + Sync {
    /// Applies `events` of `source` and sets its cursor to `next_cursor`. Returns the number of
    /// records written.
    fn apply_page(
        &self,
        source: &str,
        events: &[FeedEvent],
        next_cursor: u64,
    ) -> impl Future<Output = Result<usize, StoreError>> + Send;
}
