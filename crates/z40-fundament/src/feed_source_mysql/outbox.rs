//! Writing an event into the outbox, inside the app's transaction (the outbox pattern): the change and
//! its event commit or roll back together – no event without a change, no change without an event.

use crate::StoreError;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// One change for the outbox: the complete current state of a record (only the fields consumers may
/// see) or a tombstone.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEvent {
    /// Record type URN; one of the types of the [`super::OutboxWriter`].
    pub record_type: String,
    /// Stable id of the record, a UUID (stored in lower case).
    pub key: String,
    /// Version after the change; strictly increasing per record, at least 1.
    pub version: u64,
    /// Tombstone: the record was deleted.
    pub deleted: bool,
    /// Time of the change; `None` (the default of [`OutboxEvent::state`] and
    /// [`OutboxEvent::tombstone`]) takes the database clock when the event is written
    /// (`UTC_TIMESTAMP(3)`), so all events of a source share one clock. Set it with [`OutboxEvent::at`].
    pub occurred_at: Option<DateTime<Utc>>,
    /// The state as a JSON object; `None` for tombstones.
    pub data: Option<Value>,
}

impl OutboxEvent {
    /// The state `data` of record `key` in `version`, at the time of writing (database clock).
    pub fn state(
        record_type: impl Into<String>,
        key: impl Into<String>,
        version: u64,
        data: Value,
    ) -> Self {
        Self {
            record_type: record_type.into(),
            key: key.into(),
            version,
            deleted: false,
            occurred_at: None,
            data: Some(data),
        }
    }

    /// A tombstone of record `key` in `version`, at the time of writing (database clock).
    pub fn tombstone(record_type: impl Into<String>, key: impl Into<String>, version: u64) -> Self {
        Self {
            record_type: record_type.into(),
            key: key.into(),
            version,
            deleted: true,
            occurred_at: None,
            data: None,
        }
    }

    /// The same event at `occurred_at` (app clock) instead of the database clock.
    pub fn at(mut self, occurred_at: DateTime<Utc>) -> Self {
        self.occurred_at = Some(occurred_at);
        self
    }
}

/// Why an event was not written. Names the rule, never the contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidEvent {
    /// The type is not one of the store's types.
    #[error("record type not offered by this source")]
    UnknownType,
    /// The key is not a UUID.
    #[error("record key is not a UUID")]
    InvalidKey,
    /// The version is 0.
    #[error("version must be at least 1")]
    InvalidVersion,
    /// A state without a JSON object as data.
    #[error("a state needs a JSON object as data")]
    MissingData,
}

/// Failure of [`super::OutboxWriter::write`] and [`super::MySqlSource::write_event`].
#[derive(Debug, thiserror::Error)]
pub enum WriteEventError {
    /// The event breaks a rule; nothing was written.
    #[error(transparent)]
    Invalid(#[from] InvalidEvent),
    /// This version of the record is already in the outbox (unique key `type`, `record_key`,
    /// `version`): usually a lost update; nothing was written.
    #[error("this version of the record was already written")]
    VersionExists,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl WriteEventError {
    /// Whether the app may run its whole transaction again: the database failed transiently
    /// ([`StoreError::is_retryable`], e.g. a deadlock). A rule violation or an existing version is not.
    pub fn is_retryable(&self) -> bool {
        matches!(self, WriteEventError::Store(e) if e.is_retryable())
    }
}
