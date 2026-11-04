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
/// Rows scanned per batch (a window of consecutive ids; short transactions).
const WINDOW: u32 = 1000;

impl MySqlSource {
    /// Deletes superseded events older than `older_than` (by their `occurred_at`, e.g.
    /// [`crate::feed_source::FeedSourceConfig::retention`]) and failed-attempt counters older than
    /// [`FAILURE_RETENTION_HOURS`]. Returns the number of deleted events. Run it daily in the app's
    /// worker. It walks the outbox once in windows of consecutive ids (keyset: each batch continues after
    /// the last id of the previous one, so no batch scans the table again); each window is its own
    /// `READ COMMITTED` transaction, so writers of the outbox are not blocked.
    pub async fn purge(&self, older_than: Duration) -> Result<u64, StoreError> {
        let now = Utc::now();
        let cutoff = chrono::Duration::from_std(older_than)
            .ok()
            .and_then(|d| now.checked_sub_signed(d));
        let mut total = 0u64;
        if let Some(cutoff) = cutoff {
            let mut after = 0u64;
            while let Some((upper, n)) = self.purge_window(after, cutoff.naive_utc()).await? {
                total += n;
                after = upper;
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

    /// The last id of the window of up to [`WINDOW`] ids after `after`, or `None` at the end.
    async fn window_end(&self, after: u64) -> Result<Option<u64>, StoreError> {
        sqlx::query_scalar(self.tables.sql(
            "SELECT MAX(`id`) FROM (SELECT `id` FROM {outbox} WHERE `id` > ? ORDER BY `id` LIMIT ?) AS w",
        ))
        .bind(after)
        .bind(WINDOW)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    /// Deletes the superseded old events with an id in `(after, end]` of the next window; returns the
    /// window end and the number deleted, or `None` when no id follows `after`. The derived table lets
    /// MariaDB/MySQL delete from the table the subquery reads.
    pub(super) async fn purge_window(
        &self,
        after: u64,
        cutoff: chrono::NaiveDateTime,
    ) -> Result<Option<(u64, u64)>, StoreError> {
        let Some(end) = self.window_end(after).await? else {
            return Ok(None);
        };
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
               WHERE o.`id` > ? AND o.`id` <= ? AND o.`cursor` IS NOT NULL AND o.`occurred_at` < ? \
                 AND EXISTS (SELECT 1 FROM {outbox} n WHERE n.`type` = o.`type` \
                   AND n.`record_key` = o.`record_key` AND n.`version` > o.`version` \
                   AND n.`cursor` IS NOT NULL)) AS superseded)",
        ))
        .bind(after)
        .bind(end)
        .bind(cutoff)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(Some((end, r.rows_affected())))
    }
}
