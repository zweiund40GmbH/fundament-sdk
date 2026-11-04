//! A local replica of feed records: the app decides through a [`Mapping`] which record types it keeps
//! and which fields of `data` (by default **none** – no personal data is stored unless the app asks
//! for it); a [`ReplicaStore`] writes the resulting [`Record`]s with the version rule. The [`Replica`]
//! adapter joins both into a [`PageSink`] for [`crate::consumer::FeedConsumer::run_once`].
//!
//! * [`KeepFields`] – a ready-made mapping that keeps named fields

mod keep;

pub use keep::{KeepFields, TEXT_MAX, text_field};

use crate::consumer::{CursorState, CursorStore, FetchError, PageSink, RunLock, StoreError};
use crate::protocol::feed::FeedEvent;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::future::Future;

/// Longest stored external id; longer ids are dropped.
pub const EXTERNAL_ID_MAX: usize = 64;

/// What a mapping keeps of one record state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Projection {
    /// The kept fields; `None` = nothing.
    pub data: Option<Value>,
    /// An id of the record in the app's own data (e.g. from a list of external references), for
    /// joining the replica with local tables. At most [`EXTERNAL_ID_MAX`] characters.
    pub external_id: Option<String>,
}

/// Decides what the app keeps of the feed.
pub trait Mapping: Send + Sync {
    /// The kind stored for events of `event_type` (at most 32 characters, e.g. `customer`), or
    /// `None` to skip events of that type.
    fn kind(&self, event_type: &str) -> Option<&str>;

    /// What is kept of the state `data` of a record of `kind`. The default keeps nothing.
    fn project(&self, kind: &str, data: &Value) -> Projection {
        let _ = (kind, data);
        Projection::default()
    }
}

/// One record as written to the replica.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Kind from the mapping.
    pub kind: String,
    /// Key of the record, lower-case UUID.
    pub key: String,
    /// Version; only higher versions replace a stored record.
    pub version: u64,
    /// Tombstone: `data` and `external_id` are `None`.
    pub deleted: bool,
    /// External id from the projection.
    pub external_id: Option<String>,
    /// Kept fields from the projection.
    pub data: Option<Value>,
}

/// The record of `event` under `mapping`; `None` for types the mapping skips.
pub fn record(mapping: &impl Mapping, event: &FeedEvent) -> Option<Record> {
    let kind = mapping.kind(&event.event_type)?;
    let projection = event
        .state()
        .map(|data| mapping.project(kind, data))
        .unwrap_or_default();
    Some(Record {
        kind: kind.to_owned(),
        key: event.key.to_ascii_lowercase(),
        version: event.version,
        deleted: event.state().is_none(),
        external_id: projection
            .external_id
            .filter(|id| id.chars().count() <= EXTERNAL_ID_MAX),
        data: projection.data,
    })
}

/// Stores records of the replica.
pub trait ReplicaStore: Send + Sync {
    /// Writes every record whose version is higher than the stored one
    /// ([`crate::protocol::feed::should_apply`]; tombstones clear the data) and sets the cursor of
    /// `source` to `next_cursor` – in **one** transaction. Returns the number of records written.
    fn apply_records(
        &self,
        source: &str,
        records: &[Record],
        next_cursor: u64,
    ) -> impl Future<Output = Result<usize, StoreError>> + Send;
}

/// A store plus a mapping: implements [`PageSink`] by mapping each event and handing the records to
/// the store, and passes [`RunLock`] and [`CursorStore`] through to the store.
#[derive(Debug, Clone)]
pub struct Replica<S, M> {
    store: S,
    mapping: M,
}

impl<S, M> Replica<S, M> {
    /// Joins `store` and `mapping`.
    pub fn new(store: S, mapping: M) -> Self {
        Self { store, mapping }
    }

    /// The store, e.g. for reading the replica.
    pub fn store(&self) -> &S {
        &self.store
    }
}

impl<S: ReplicaStore, M: Mapping> PageSink for Replica<S, M> {
    async fn apply_page(
        &self,
        source: &str,
        events: &[FeedEvent],
        next_cursor: u64,
    ) -> Result<usize, StoreError> {
        let records: Vec<Record> = events
            .iter()
            .filter_map(|e| record(&self.mapping, e))
            .collect();
        self.store
            .apply_records(source, &records, next_cursor)
            .await
    }
}

impl<S: RunLock, M: Send + Sync> RunLock for Replica<S, M> {
    type Guard = S::Guard;

    fn try_lock(
        &self,
        source: &str,
    ) -> impl Future<Output = Result<Option<Self::Guard>, StoreError>> + Send {
        self.store.try_lock(source)
    }

    fn unlock(&self, guard: Self::Guard) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.store.unlock(guard)
    }
}

impl<S: CursorStore, M: Send + Sync> CursorStore for Replica<S, M> {
    fn load(
        &self,
        source: &str,
    ) -> impl Future<Output = Result<Option<CursorState>, StoreError>> + Send {
        self.store.load(source)
    }

    fn reset(&self, source: &str) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.store.reset(source)
    }

    fn finish_run(
        &self,
        source: &str,
        error: Option<FetchError>,
        at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.store.finish_run(source, error, at)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
