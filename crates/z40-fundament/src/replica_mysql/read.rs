//! Reading what [`MySqlStore`] wrote: records of the replica and the newest events (for the status
//! document). Stored JSON that cannot be parsed reads as `None` (the next newer version repairs it).

use super::MySqlStore;
use crate::consumer::StoreError;
use crate::protocol::Level;
use crate::sql::db;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;

/// One stored record.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    /// Kind from the mapping.
    pub kind: String,
    /// Key (lower-case UUID).
    pub key: String,
    /// Stored version.
    pub version: u64,
    /// Tombstone.
    pub deleted: bool,
    /// External id from the mapping.
    pub external_id: Option<String>,
    /// Kept fields.
    pub data: Option<Value>,
    /// When the row was last written.
    pub updated_at: DateTime<Utc>,
}

/// One stored event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// Time.
    pub time: DateTime<Utc>,
    /// Kind, e.g. `feed`.
    pub kind: String,
    /// Level.
    pub level: Level,
    /// Fixed text.
    pub text: String,
}

/// A row of the replica table.
type Row = (
    String,
    String,
    u64,
    bool,
    Option<String>,
    Option<String>,
    NaiveDateTime,
);

/// Columns of [`Row`].
const COLUMNS: &str =
    "`kind`, `record_key`, `version`, `deleted`, `external_id`, `data`, `updated_at`";

fn stored((kind, key, version, deleted, external_id, data, updated_at): Row) -> StoredRecord {
    StoredRecord {
        kind,
        key,
        version,
        deleted,
        external_id,
        data: data.and_then(|d| serde_json::from_str(&d).ok()),
        updated_at: updated_at.and_utc(),
    }
}

impl MySqlStore {
    /// The stored record `key` of `kind` from `source` (also a tombstone), or `None`.
    pub async fn record(
        &self,
        source: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<StoredRecord>, StoreError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM {{replica}} WHERE `source` = ? AND `kind` = ? AND `record_key` = ?"
        );
        let row: Option<Row> = sqlx::query_as(self.tables.sql(&sql))
            .bind(source)
            .bind(kind)
            .bind(key.to_ascii_lowercase())
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        Ok(row.map(stored))
    }

    /// All records of `kind` from `source` that are not deleted, ordered by key.
    pub async fn records(&self, source: &str, kind: &str) -> Result<Vec<StoredRecord>, StoreError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM {{replica}} WHERE `source` = ? AND `kind` = ? AND `deleted` = 0 \
             ORDER BY `record_key`"
        );
        let rows: Vec<Row> = sqlx::query_as(self.tables.sql(&sql))
            .bind(source)
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        Ok(rows.into_iter().map(stored).collect())
    }

    /// The newest `max` events since `since`, newest first. `max` is any integer, e.g.
    /// `status::MAX_EVENTS` (`usize`) or a `u32`; negative values count as 0, values above `u32::MAX`
    /// as `u32::MAX`.
    pub async fn recent_events<N>(
        &self,
        since: DateTime<Utc>,
        max: N,
    ) -> Result<Vec<StoredEvent>, StoreError>
    where
        N: TryInto<u32> + PartialOrd + Default + Copy,
    {
        let max: u32 = max
            .try_into()
            .unwrap_or(if max < N::default() { 0 } else { u32::MAX });
        let rows: Vec<(NaiveDateTime, String, String, String)> = sqlx::query_as(self.tables.sql(
            "SELECT `time`, `kind`, `level`, `text` FROM {event} WHERE `time` >= ? \
             ORDER BY `time` DESC, `id` DESC LIMIT ?",
        ))
        .bind(since.naive_utc())
        .bind(max)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|(time, kind, level, text)| StoredEvent {
                time: time.and_utc(),
                kind,
                level: Level::from_code(&level),
                text,
            })
            .collect())
    }
}
