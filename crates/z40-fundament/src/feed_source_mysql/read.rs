//! [`FeedSourceStore`] for [`MySqlSource`]: reading committed events with a cursor, the last assigned
//! cursor, and delegating to the assigner and the poll log.

use super::MySqlSource;
use crate::StoreError;
use crate::feed_source::{AssignOutcome, FeedSourceStore};
use crate::protocol::feed::FeedEvent;
use crate::protocol::timestamp;
use crate::sql::db;
use chrono::{DateTime, NaiveDateTime, Utc};
use std::time::Duration;

/// Stored data that is not JSON (the schema's check constraint was removed or bypassed).
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("stored event data is not valid JSON")]
struct CorruptData;

/// A row of the outbox as read for the feed.
type Row = (
    u64,
    u64,
    String,
    String,
    u64,
    bool,
    NaiveDateTime,
    Option<String>,
);

/// The event of a row. A tombstone never carries data.
fn event(
    (id, cursor, record_type, key, version, deleted, occurred_at, data): Row,
) -> Result<FeedEvent, StoreError> {
    let data = match data {
        Some(d) if !deleted => {
            Some(serde_json::from_str(&d).map_err(|_| StoreError::new(CorruptData))?)
        }
        _ => None,
    };
    Ok(FeedEvent {
        cursor,
        id: Some(id.to_string()),
        event_type: record_type,
        key,
        version,
        deleted,
        occurred_at: Some(timestamp(occurred_at.and_utc())),
        data,
    })
}

impl FeedSourceStore for MySqlSource {
    fn known_types(&self) -> &[String] {
        &self.types
    }

    async fn assign_cursors(&self, max_wait: Duration) -> Result<AssignOutcome, StoreError> {
        self.assign(max_wait).await
    }

    async fn last_cursor(&self) -> Result<Option<u64>, StoreError> {
        let last: Option<u64> = sqlx::query_scalar(
            self.tables
                .sql("SELECT `last` FROM {counter} WHERE `id` = 1"),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(last)
    }

    async fn read(
        &self,
        since: u64,
        limit: u32,
        types: &[String],
    ) -> Result<Vec<FeedEvent>, StoreError> {
        if types.is_empty() {
            return Ok(Vec::new());
        }
        let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
            self.tables
                .sql(
                    "SELECT `id`, `cursor`, `type`, `record_key`, `version`, `deleted`, `occurred_at`, \
                     `data` FROM {outbox} WHERE `cursor` > ",
                )
                .0,
        );
        q.push_bind(since).push(" AND `type` IN (");
        let mut list = q.separated(", ");
        for t in types {
            list.push_bind(t);
        }
        q.push(") ORDER BY `cursor` LIMIT ").push_bind(limit);
        let rows: Vec<Row> = q.build_query_as().fetch_all(&self.pool).await.map_err(db)?;
        rows.into_iter().map(event).collect()
    }

    async fn record_poll(
        &self,
        consumer: &str,
        next_cursor: u64,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.remember_poll(consumer, next_cursor, at).await
    }

    async fn record_failures(&self, count: u64, at: DateTime<Utc>) -> Result<(), StoreError> {
        self.count_failures(count, at).await
    }
}
