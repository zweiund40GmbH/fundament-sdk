//! The retention of the outbox (`docs/protocol.md` §2.3): only events that are older than the retention
//! **and** superseded by a newer version of the same record (with a cursor) are deleted. The newest state
//! of every record – also a tombstone – always stays, and events without a cursor are never touched.
//! `since=0` therefore remains a complete initial sync, and the feed never reports `gap`.

use super::MySqlSource;
use crate::StoreError;
use crate::sql::db;
use chrono::{DurationRound, Utc};
use std::time::Duration;

/// Hours the failed-attempt counters are kept (the status reports 24 hours).
pub const FAILURE_RETENTION_HOURS: i64 = 48;
/// Events deleted per statement (short transactions).
const BATCH: u32 = 1000;

impl MySqlSource {
    /// Deletes superseded events older than `older_than` (e.g. [`crate::feed_source::FeedSourceConfig::retention`])
    /// and failed-attempt counters older than [`FAILURE_RETENTION_HOURS`]. Returns the number of deleted
    /// events. Run it daily in the app's worker; each batch is its own `READ COMMITTED` transaction, so
    /// writers of the outbox are not blocked.
    pub async fn purge(&self, older_than: Duration) -> Result<u64, StoreError> {
        let now = Utc::now();
        let cutoff = chrono::Duration::from_std(older_than)
            .ok()
            .and_then(|d| now.checked_sub_signed(d));
        let mut total = 0u64;
        if let Some(cutoff) = cutoff {
            loop {
                let n = self.purge_batch(cutoff.naive_utc()).await?;
                total += n;
                if n < u64::from(BATCH) {
                    break;
                }
            }
        }
        let hour = now
            .duration_trunc(chrono::Duration::hours(1))
            .unwrap_or(now)
            - chrono::Duration::hours(FAILURE_RETENTION_HOURS);
        sqlx::query(self.tables.sql("DELETE FROM {failure} WHERE `hour` < ?"))
            .bind(hour.naive_utc())
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(total)
    }

    /// One batch of [`MySqlSource::purge`]. The derived table lets MariaDB delete from the table it reads.
    async fn purge_batch(&self, cutoff: chrono::NaiveDateTime) -> Result<u64, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(db)?;
        // Applies to the next transaction of this connection only.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        let mut tx = sqlx::Connection::begin(&mut *conn).await.map_err(db)?;
        let r = sqlx::query(self.tables.sql(
            "DELETE FROM {outbox} WHERE `id` IN (SELECT `id` FROM (\
               SELECT o.`id` FROM {outbox} o \
               WHERE o.`cursor` IS NOT NULL AND o.`occurred_at` < ? \
                 AND EXISTS (SELECT 1 FROM {outbox} n WHERE n.`type` = o.`type` \
                   AND n.`record_key` = o.`record_key` AND n.`version` > o.`version` \
                   AND n.`cursor` IS NOT NULL) \
               LIMIT ?) AS superseded)",
        ))
        .bind(cutoff)
        .bind(BATCH)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(r.rows_affected())
    }
}
