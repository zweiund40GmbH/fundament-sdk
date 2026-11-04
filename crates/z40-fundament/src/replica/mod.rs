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
/// Longest kind (column width of the replica and event tables).
pub const KIND_MAX: usize = 32;

/// A kind is empty or longer than [`KIND_MAX`] characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("kind must be 1 to 32 characters")]
pub struct InvalidKind;

/// Checks a kind (`Ok` if it has 1 to [`KIND_MAX`] characters).
pub fn check_kind(kind: &str) -> Result<(), InvalidKind> {
    let n = kind.chars().count();
    if (1..=KIND_MAX).contains(&n) {
        Ok(())
    } else {
        Err(InvalidKind)
    }
}

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
    /// The kind stored for events of `event_type` (1 to [`KIND_MAX`] characters, e.g. `customer`), or
    /// `None` to skip events of that type. Several types may share a kind (e.g. `…customer` and
    /// `…customer.v2` during a transition).
    fn kind(&self, event_type: &str) -> Option<&str>;

    /// What is kept of the state `data` of an event of `event_type`. Keyed by the type, not the kind,
    /// so types sharing a kind can differ in their fields. The default keeps nothing.
    fn project(&self, event_type: &str, data: &Value) -> Projection {
        let _ = (event_type, data);
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

/// The record of `event` under `mapping`; `Ok(None)` for types the mapping skips, `Err` when the
/// mapping returns an invalid kind.
pub fn record(mapping: &impl Mapping, event: &FeedEvent) -> Result<Option<Record>, InvalidKind> {
    let Some(kind) = mapping.kind(&event.event_type) else {
        return Ok(None);
    };
    check_kind(kind)?;
    let projection = event
        .state()
        .map(|data| mapping.project(&event.event_type, data))
        .unwrap_or_default();
    Ok(Some(Record {
        kind: kind.to_owned(),
        key: event.key.to_ascii_lowercase(),
        version: event.version,
        deleted: event.state().is_none(),
        external_id: projection
            .external_id
            .filter(|id| id.chars().count() <= EXTERNAL_ID_MAX),
        data: projection.data,
    }))
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
        let mut records = Vec::with_capacity(events.len());
        for e in events {
            if let Some(r) = record(&self.mapping, e).map_err(StoreError::new)? {
                records.push(r);
            }
        }
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
