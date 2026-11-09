//! [`MySqlLoginLimitStore`]: the counters of the shared login limit in the table `bff_login_limit`.

use super::SessionTables;
use super::error::db;
use crate::bff::{LoginLimitStore, TokenHash};
use chrono::{DateTime, SubsecRound, Utc};
use sqlx::MySqlPool;
use z40_fundament::StoreError;

/// Rows deleted per statement of the cleanup.
const CLEANUP_BATCH: u64 = 1000;

/// The login limit counters of all instances in MariaDB/MySQL (see
/// [`crate::bff::BffBuilder::login_limit_store`]). Cheap to clone (the pool is shared).
#[derive(Debug, Clone)]
pub struct MySqlLoginLimitStore {
    pool: MySqlPool,
    tables: SessionTables,
}

impl MySqlLoginLimitStore {
    /// A store over `pool` with the login limit table of `tables`.
    pub fn new(pool: MySqlPool, tables: SessionTables) -> Self {
        Self { pool, tables }
    }

    /// Deletes the counters of windows that ended at or before `now`, in batches; returns the number of
    /// rows removed. Call it from the app's worker with [`super::MySqlSessionStore::delete_expired`].
    pub async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let mut total = 0;
        loop {
            let sql = "DELETE FROM {login_limit} WHERE `expires_at` <= ? LIMIT ?";
            let done = sqlx::query(self.tables.sql(sql))
                .bind(now.naive_utc().trunc_subsecs(3))
                .bind(CLEANUP_BATCH)
                .execute(&self.pool)
                .await
                .map_err(db)?
                .rows_affected();
            total += done;
            if done < CLEANUP_BATCH {
                return Ok(total);
            }
        }
    }
}

impl LoginLimitStore for MySqlLoginLimitStore {
    /// `INSERT … ON DUPLICATE KEY UPDATE` and a read of the row in one transaction: the row stays
    /// locked until the commit, so each call reads its own count.
    async fn increment(
        &self,
        key: &TokenHash,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let start = window_start.naive_utc().trunc_subsecs(0);
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query(self.tables.sql(
            "INSERT INTO {login_limit} (`key_hash`, `window_start`, `count`, `expires_at`) \
             VALUES (?, ?, 1, ?) ON DUPLICATE KEY UPDATE `count` = `count` + 1",
        ))
        .bind(&key.as_bytes()[..])
        .bind(start)
        .bind(window_end.naive_utc().trunc_subsecs(3))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let (count,): (u64,) =
            sqlx::query_as(self.tables.sql(
                "SELECT `count` FROM {login_limit} WHERE `key_hash` = ? AND `window_start` = ?",
            ))
            .bind(&key.as_bytes()[..])
            .bind(start)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(count)
    }
}
