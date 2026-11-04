//! Writing an event into the outbox, inside the app's transaction (the outbox pattern): the change and
//! its event commit or roll back together – no event without a change, no change without an event.

use super::MySqlSource;
use crate::StoreError;
use crate::protocol::feed::is_uuid;
use crate::sql::db;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{MySql, Transaction};

/// One change for the outbox: the complete current state of a record (only the fields consumers may
/// see) or a tombstone.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEvent {
    /// Record type URN; one of the types of the [`MySqlSource`].
    pub record_type: String,
    /// Stable id of the record, a UUID (stored in lower case).
    pub key: String,
    /// Version after the change; strictly increasing per record, at least 1.
    pub version: u64,
    /// Tombstone: the record was deleted.
    pub deleted: bool,
    /// Time of the change.
    pub occurred_at: DateTime<Utc>,
    /// The state as a JSON object; `None` for tombstones.
    pub data: Option<Value>,
}

impl OutboxEvent {
    /// The state `data` of record `key` in `version`, now.
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
            occurred_at: Utc::now(),
            data: Some(data),
        }
    }

    /// A tombstone of record `key` in `version`, now.
    pub fn tombstone(record_type: impl Into<String>, key: impl Into<String>, version: u64) -> Self {
        Self {
            record_type: record_type.into(),
            key: key.into(),
            version,
            deleted: true,
            occurred_at: Utc::now(),
            data: None,
        }
    }

    /// The same event at `occurred_at`.
    pub fn at(mut self, occurred_at: DateTime<Utc>) -> Self {
        self.occurred_at = occurred_at;
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

/// Failure of [`MySqlSource::write_event`].
#[derive(Debug, thiserror::Error)]
pub enum WriteEventError {
    /// The event breaks a rule; nothing was written.
    #[error(transparent)]
    Invalid(#[from] InvalidEvent),
    /// The database failed (also: the version of this record was already written).
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl MySqlSource {
    /// Checks `e` against the rules of the protocol and the types of this store.
    pub(super) fn check(&self, e: &OutboxEvent) -> Result<(), InvalidEvent> {
        if !self.types.contains(&e.record_type) {
            return Err(InvalidEvent::UnknownType);
        }
        if !is_uuid(&e.key) {
            return Err(InvalidEvent::InvalidKey);
        }
        if e.version == 0 {
            return Err(InvalidEvent::InvalidVersion);
        }
        if !e.deleted && !e.data.as_ref().is_some_and(Value::is_object) {
            return Err(InvalidEvent::MissingData);
        }
        Ok(())
    }

    /// Writes `event` into the outbox **in the app's transaction `tx`**, together with the change it
    /// describes; the app commits or rolls back. The event gets its cursor after the commit (cursor
    /// assigner). A tombstone's data is never stored. Writing the same version of a record twice fails
    /// (unique key), so a lost update shows instead of two states with one version.
    pub async fn write_event(
        &self,
        tx: &mut Transaction<'_, MySql>,
        event: &OutboxEvent,
    ) -> Result<(), WriteEventError> {
        self.check(event)?;
        let data = if event.deleted {
            None
        } else {
            event.data.as_ref().map(Value::to_string)
        };
        sqlx::query(self.tables.sql(
            "INSERT INTO {outbox} (`type`, `record_key`, `version`, `deleted`, `occurred_at`, `data`) \
             VALUES (?, ?, ?, ?, ?, ?)",
        ))
        .bind(&event.record_type)
        .bind(event.key.to_ascii_lowercase())
        .bind(event.version)
        .bind(event.deleted)
        .bind(event.occurred_at.naive_utc())
        .bind(data)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
        Ok(())
    }
}
