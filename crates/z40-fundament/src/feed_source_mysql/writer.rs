//! [`OutboxWriter`]: writing events into the outbox without a pool. Apps that only write events (in
//! their own transactions) need neither a [`super::MySqlSource`] nor its pool; the source delegates
//! to the same writer.

use super::outbox::{InvalidEvent, OutboxEvent, WriteEventError};
use super::{InvalidType, SourceTables};
use crate::StoreErrorKind;
use crate::feed_source::is_valid_type;
use crate::protocol::feed::is_uuid;
use crate::sql::db;
use serde_json::Value;
use sqlx::{MySql, Transaction};

/// Writes events into the outbox of `tables` inside the app's transactions. Holds no connection;
/// cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxWriter {
    pub(super) tables: SourceTables,
    pub(super) types: Vec<String>,
}

impl OutboxWriter {
    /// A writer for the outbox of `tables`, accepting the record types `types` (URNs, each once).
    pub fn new(
        tables: SourceTables,
        types: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, InvalidType> {
        let mut checked: Vec<String> = Vec::new();
        for t in types.into_iter().map(Into::into) {
            if !is_valid_type(&t) || checked.contains(&t) {
                return Err(InvalidType);
            }
            checked.push(t);
        }
        Ok(Self {
            tables,
            types: checked,
        })
    }

    /// The table names.
    pub fn tables(&self) -> &SourceTables {
        &self.tables
    }

    /// The accepted record types.
    pub fn types(&self) -> &[String] {
        &self.types
    }

    /// Checks `e` against the rules of the protocol and the types of this writer.
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
    /// assigner). A tombstone's data is never stored. Without an explicit time the database clock
    /// fills `occurred_at`. Writing the same version of a record twice is
    /// [`WriteEventError::VersionExists`], so a lost update shows instead of two states with one
    /// version; a deadlock or lock wait timeout is [`WriteEventError::is_retryable`].
    pub async fn write(
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
             VALUES (?, ?, ?, ?, COALESCE(?, UTC_TIMESTAMP(3)), ?)",
        ))
        .bind(&event.record_type)
        .bind(event.key.to_ascii_lowercase())
        .bind(event.version)
        .bind(event.deleted)
        .bind(event.occurred_at.map(|t| t.naive_utc()))
        .bind(data)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            // The only unique key an insert can hit is the version key (`id` is generated, `cursor`
            // is `NULL`), so a conflict is an existing version.
            let e = db(e);
            if e.kind() == StoreErrorKind::Conflict {
                WriteEventError::VersionExists
            } else {
                WriteEventError::Store(e)
            }
        })?;
        Ok(())
    }
}
