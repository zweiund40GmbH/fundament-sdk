//! The cursor assigner. One round:
//!
//! 1. takes `GET_LOCK('<prefix>fundament-cursor:<database>', 0)` on a **new connection per round**; if
//!    another instance holds it, the round is skipped (what that instance assigns is visible to
//!    everyone). The connection is marked `close_on_drop` *before* `GET_LOCK`, so it never goes back to
//!    the pool: neither the lock nor the session settings below can leak – also when the round is
//!    cancelled while `GET_LOCK` is on its way. This costs one reconnect per round.
//! 2. numbers events in batches, each in one `READ COMMITTED` transaction (no gap locks, so writers of
//!    the outbox are never blocked): lock the counter row, lock the events without cursor in the order of
//!    origin (`id`), give them `last + 1 …`, move the counter, commit. The new cursors become visible all
//!    at once with the commit.
//!
//! The locking read **waits** for an event of a still open transaction instead of numbering a later
//! event first (origin order across records) – at most the `max_wait` of the round, rounded up to whole
//! seconds: [`WORKER_ASSIGN_WAIT`](crate::feed_source::WORKER_ASSIGN_WAIT) in the worker,
//! [`REQUEST_ASSIGN_WAIT`](crate::feed_source::REQUEST_ASSIGN_WAIT) before a feed answer. Then the batch is given up and the round ends with what is
//! assigned ([`AssignOutcome::Waiting`]); the open events get a higher cursor in a later round. No event
//! is ever skipped, and a consumer following `nextCursor` never misses one.

use super::MySqlSource;
use crate::StoreError;
use crate::feed_source::AssignOutcome;
use crate::sql::db;
use sqlx::mysql::MySqlDatabaseError;
use sqlx::{Connection, MySqlConnection};
use std::time::Duration;

/// Events numbered per transaction.
const BATCH: usize = 1000;
/// Batches per round (bounded work per feed request).
const ROUNDS: usize = 10;
/// MariaDB/MySQL error number of a lock wait timeout.
const ER_LOCK_WAIT_TIMEOUT: u16 = 1205;

/// The counter row is missing (the `INSERT` of `schema/mysql-source.sql` did not run).
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("the counter row of the outbox is missing; run the schema's INSERT")]
struct MissingCounter;

/// Whether `e` is a lock wait timeout.
fn is_lock_wait_timeout(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.try_downcast_ref::<MySqlDatabaseError>())
        .is_some_and(|m| m.number() == ER_LOCK_WAIT_TIMEOUT)
}

impl MySqlSource {
    /// One round of the assigner (see the module docs).
    pub(super) async fn assign(&self, max_wait: Duration) -> Result<AssignOutcome, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(db)?;
        // Before GET_LOCK: a round cancelled while the lock is being taken must not return a session
        // that holds it to the pool.
        conn.close_on_drop();
        let got: Option<i64> =
            sqlx::query_scalar("SELECT GET_LOCK(LEFT(CONCAT(?, ':', DATABASE()), 64), 0)")
                .bind(&self.writer.tables.lock)
                .fetch_one(&mut *conn)
                .await
                .map_err(db)?;
        if got != Some(1) {
            return Ok(AssignOutcome::Skipped);
        }
        // Session settings of this connection only; it is closed after the round. The value is an
        // integer computed here (SET takes no bind parameter); InnoDB's minimum is 1 second.
        let secs = (max_wait.as_secs() + u64::from(max_wait.subsec_nanos() > 0)).clamp(1, 3600);
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SET SESSION innodb_lock_wait_timeout = {secs}"
        )))
        .execute(&mut *conn)
        .await
        .map_err(db)?;
        sqlx::query("SET SESSION TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        let mut total = 0u64;
        for _ in 0..ROUNDS {
            match self.batch(&mut conn).await {
                Ok(n) => {
                    total += n as u64;
                    if n < BATCH {
                        break;
                    }
                }
                Err(e) if is_lock_wait_timeout(&e) => return Ok(AssignOutcome::Waiting(total)),
                Err(e) => return Err(db(e)),
            }
        }
        // Closing the connection on drop releases the lock anyway; releasing explicitly is tidier.
        let _ = sqlx::query("SELECT RELEASE_LOCK(LEFT(CONCAT(?, ':', DATABASE()), 64))")
            .bind(&self.writer.tables.lock)
            .execute(&mut *conn)
            .await;
        Ok(AssignOutcome::Assigned(total))
    }

    /// Numbers up to [`BATCH`] events in one transaction; returns how many.
    async fn batch(&self, conn: &mut MySqlConnection) -> Result<usize, sqlx::Error> {
        let mut tx = conn.begin().await?;
        let last: Option<u64> = sqlx::query_scalar(
            self.writer
                .tables
                .sql("SELECT `last` FROM {counter} WHERE `id` = 1 FOR UPDATE"),
        )
        .fetch_optional(&mut *tx)
        .await?;
        let last = last.ok_or_else(|| sqlx::Error::Decode(Box::new(MissingCounter)))?;
        // `(cursor, id)` holds the unassigned rows (`NULL`) in `id` order: the locking read stops after
        // `BATCH` rows (no filesort, so only those rows are locked) and waits only when an open
        // transaction's row is among the first `BATCH` unassigned ones.
        let ids: Vec<u64> = sqlx::query_scalar(self.writer.tables.sql(
            "SELECT `id` FROM {outbox} FORCE INDEX (`ix_fundament_outbox_unassigned`) \
             WHERE `cursor` IS NULL ORDER BY `id` LIMIT ? FOR UPDATE",
        ))
        .bind(BATCH as u64)
        .fetch_all(&mut *tx)
        .await?;
        if !ids.is_empty() {
            self.number(&mut tx, &ids, last).await?;
        }
        tx.commit().await?;
        Ok(ids.len())
    }

    /// Gives the locked events `ids` (ascending) the cursors `last + 1 …` in that order and moves the
    /// counter. `FIELD` is the 1-based position of `id` in the list; every list entry is a bind parameter.
    async fn number(
        &self,
        tx: &mut MySqlConnection,
        ids: &[u64],
        last: u64,
    ) -> Result<(), sqlx::Error> {
        let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
            self.writer.tables.sql("UPDATE {outbox} SET `cursor` = ").0,
        );
        q.push_bind(last);
        q.push(" + FIELD(`id`");
        for id in ids {
            q.push(", ").push_bind(*id);
        }
        q.push(") WHERE `id` IN (");
        let mut list = q.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
        q.push(")");
        q.build().execute(&mut *tx).await?;
        sqlx::query(
            self.writer
                .tables
                .sql("UPDATE {counter} SET `last` = ? WHERE `id` = 1"),
        )
        .bind(last + ids.len() as u64)
        .execute(&mut *tx)
        .await?;
        Ok(())
    }
}
