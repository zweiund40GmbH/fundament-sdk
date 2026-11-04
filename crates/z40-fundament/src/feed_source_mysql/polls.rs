//! The poll log and the failed attempts per hour, and [`SourceStatusStore`] for [`MySqlSource`]. No row
//! refers to a person: key names, cursors and counters only. Every query is bounded by an index or a
//! `LIMIT`, so the status answers in time.

use super::MySqlSource;
use crate::StoreError;
use crate::feed_source::{OfferFigures, PollRecord, SourceStatusStore};
use crate::sql::db;
use chrono::{DateTime, DurationRound, NaiveDateTime, Utc};

/// The start of the hour of `t`.
fn hour(t: DateTime<Utc>) -> NaiveDateTime {
    t.duration_trunc(chrono::Duration::hours(1))
        .unwrap_or(t)
        .naive_utc()
}

impl MySqlSource {
    /// Remembers the last poll of `consumer`: its time and the `nextCursor` of the answer.
    pub(super) async fn remember_poll(
        &self,
        consumer: &str,
        cursor: u64,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(self.tables.sql(
            "INSERT INTO {poll} (`consumer`, `last_poll`, `cursor`) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE `last_poll` = VALUES(`last_poll`), `cursor` = VALUES(`cursor`)",
        ))
        .bind(consumer)
        .bind(at.naive_utc())
        .bind(cursor)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    /// Adds `count` failed attempts to the row of the hour of `at`.
    pub(super) async fn count_failures(
        &self,
        count: u64,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        sqlx::query(self.tables.sql(
            "INSERT INTO {failure} (`hour`, `attempts`) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE `attempts` = LEAST(`attempts` + VALUES(`attempts`), 4294967295)",
        ))
        .bind(hour(at))
        .bind(count)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }
}

impl SourceStatusStore for MySqlSource {
    /// Two queries per known type over the indexes `(type, cursor)` and `(type, occurred_at)`.
    async fn offer_figures(&self, now: DateTime<Utc>) -> Result<Vec<OfferFigures>, StoreError> {
        let mut out = Vec::with_capacity(self.types.len());
        for record_type in &self.types {
            let last: Option<u64> = sqlx::query_scalar(
                self.tables
                    .sql("SELECT MAX(`cursor`) FROM {outbox} WHERE `type` = ?"),
            )
            .bind(record_type)
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
            let (h24, m5): (i64, i64) = sqlx::query_as(self.tables.sql(
                "SELECT COUNT(*), CAST(COALESCE(SUM(`occurred_at` > ?), 0) AS SIGNED) \
                 FROM {outbox} WHERE `type` = ? AND `occurred_at` > ?",
            ))
            .bind((now - chrono::Duration::minutes(5)).naive_utc())
            .bind(record_type)
            .bind((now - chrono::Duration::hours(24)).naive_utc())
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
            out.push(OfferFigures {
                record_type: record_type.clone(),
                last_cursor: last.unwrap_or(0),
                last_5_minutes: u64::try_from(m5).unwrap_or(0),
                last_24_hours: u64::try_from(h24).unwrap_or(0),
            });
        }
        Ok(out)
    }

    async fn polls(&self) -> Result<Vec<PollRecord>, StoreError> {
        let rows: Vec<(String, NaiveDateTime, u64)> = sqlx::query_as(
            self.tables
                .sql("SELECT `consumer`, `last_poll`, `cursor` FROM {poll} ORDER BY `consumer`"),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|(consumer, last_poll, cursor)| PollRecord {
                consumer,
                last_poll: last_poll.and_utc(),
                cursor,
            })
            .collect())
    }

    async fn backlog(&self, cursor: u64, cap: u64) -> Result<u64, StoreError> {
        let n: i64 =
            sqlx::query_scalar(self.tables.sql(
                "SELECT COUNT(*) FROM (SELECT 1 FROM {outbox} WHERE `cursor` > ? LIMIT ?) AS b",
            ))
            .bind(cursor)
            .bind(cap)
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    async fn failed_attempts(&self, since: DateTime<Utc>) -> Result<u64, StoreError> {
        let n: i64 = sqlx::query_scalar(self.tables.sql(
            "SELECT CAST(COALESCE(SUM(`attempts`), 0) AS SIGNED) FROM {failure} WHERE `hour` >= ?",
        ))
        .bind(hour(since))
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }
}
