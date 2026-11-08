//! [`SessionStore`] for [`MySqlSessionStore`] and the expiry cleanup.

use super::MySqlSessionStore;
use crate::bff::{PendingLogin, SessionRecord, SessionStore, TokenHash};
use chrono::{DateTime, NaiveDateTime, SubsecRound, Utc};
use z40_fundament::StoreError;

/// Rows deleted per statement of the cleanup (keeps each statement short).
const CLEANUP_BATCH: u64 = 1000;

/// Wraps a database error.
fn db(e: sqlx::Error) -> StoreError {
    StoreError::new(e)
}

/// `t` cut to milliseconds (`DATETIME(3)`), so MySQL's rounding never moves an end later.
fn ms(t: DateTime<Utc>) -> NaiveDateTime {
    t.naive_utc().trunc_subsecs(3)
}

/// A row of the session table.
type SessionRow = (
    String,
    Option<String>,
    String,
    NaiveDateTime,
    NaiveDateTime,
    NaiveDateTime,
);

impl MySqlSessionStore {
    /// Deletes sessions and started logins that ended at or before `now`, in batches; returns the number
    /// of rows removed. Call it from the app's worker (e.g. every few minutes).
    pub async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let mut total = 0;
        for template in [
            "DELETE FROM {session} WHERE `expires_at` <= ? LIMIT ?",
            "DELETE FROM {login} WHERE `expires_at` <= ? LIMIT ?",
        ] {
            loop {
                let done = sqlx::query(self.tables.sql(template))
                    .bind(ms(now))
                    .bind(CLEANUP_BATCH)
                    .execute(&self.pool)
                    .await
                    .map_err(db)?
                    .rows_affected();
                total += done;
                if done < CLEANUP_BATCH {
                    break;
                }
            }
        }
        Ok(total)
    }
}

impl SessionStore for MySqlSessionStore {
    /// One `INSERT`.
    async fn create(&self, id: &TokenHash, s: &SessionRecord) -> Result<(), StoreError> {
        sqlx::query(self.tables.sql(
            "INSERT INTO {session} (`id_hash`, `sub`, `sid`, `data`, `created_at`, `last_seen_at`, \
             `expires_at`) VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&id.as_bytes()[..])
        .bind(&s.sub)
        .bind(&s.sid)
        .bind(&s.data)
        .bind(ms(s.created_at))
        .bind(ms(s.last_seen_at))
        .bind(ms(s.expires_at))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    /// By primary key.
    async fn load(&self, id: &TokenHash) -> Result<Option<SessionRecord>, StoreError> {
        let row: Option<SessionRow> = sqlx::query_as(self.tables.sql(
            "SELECT `sub`, `sid`, `data`, `created_at`, `last_seen_at`, `expires_at` \
             FROM {session} WHERE `id_hash` = ?",
        ))
        .bind(&id.as_bytes()[..])
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(
            row.map(|(sub, sid, data, created, seen, expires)| SessionRecord {
                sub,
                sid,
                data,
                created_at: created.and_utc(),
                last_seen_at: seen.and_utc(),
                expires_at: expires.and_utc(),
            }),
        )
    }

    /// One `UPDATE` by primary key.
    async fn touch(
        &self,
        id: &TokenHash,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            self.tables.sql(
                "UPDATE {session} SET `last_seen_at` = ?, `expires_at` = ? WHERE `id_hash` = ?",
            ),
        )
        .bind(ms(last_seen_at))
        .bind(ms(expires_at))
        .bind(&id.as_bytes()[..])
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    /// By primary key.
    async fn delete(&self, id: &TokenHash) -> Result<bool, StoreError> {
        let r = sqlx::query(self.tables.sql("DELETE FROM {session} WHERE `id_hash` = ?"))
            .bind(&id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(r.rows_affected() > 0)
    }

    /// Over the `sid` index.
    async fn delete_by_sid(&self, sid: &str) -> Result<u64, StoreError> {
        let r = sqlx::query(self.tables.sql("DELETE FROM {session} WHERE `sid` = ?"))
            .bind(sid)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(r.rows_affected())
    }

    /// Over the `sub` index.
    async fn delete_by_sub(&self, sub: &str) -> Result<u64, StoreError> {
        let r = sqlx::query(self.tables.sql("DELETE FROM {session} WHERE `sub` = ?"))
            .bind(sub)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(r.rows_affected())
    }

    /// Over the `sub` index.
    async fn delete_by_sub_without_sid(&self, sub: &str) -> Result<u64, StoreError> {
        let r = sqlx::query(
            self.tables
                .sql("DELETE FROM {session} WHERE `sub` = ? AND `sid` IS NULL"),
        )
        .bind(sub)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(r.rows_affected())
    }

    /// One `INSERT`.
    async fn put_login(&self, id: &TokenHash, login: &PendingLogin) -> Result<(), StoreError> {
        sqlx::query(self.tables.sql(
            "INSERT INTO {login} (`id_hash`, `state`, `return_to`, `expires_at`) VALUES (?, ?, ?, ?)",
        ))
        .bind(&id.as_bytes()[..])
        .bind(&login.state)
        .bind(&login.return_to)
        .bind(ms(login.expires_at))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    /// `SELECT … FOR UPDATE` and `DELETE` in one transaction: of two concurrent callbacks only one gets
    /// the row.
    async fn take_login(&self, id: &TokenHash) -> Result<Option<PendingLogin>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row: Option<(Vec<u8>, String, NaiveDateTime)> = sqlx::query_as(self.tables.sql(
            "SELECT `state`, `return_to`, `expires_at` FROM {login} WHERE `id_hash` = ? FOR UPDATE",
        ))
        .bind(&id.as_bytes()[..])
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if row.is_some() {
            sqlx::query(self.tables.sql("DELETE FROM {login} WHERE `id_hash` = ?"))
                .bind(&id.as_bytes()[..])
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(row.map(|(state, return_to, expires)| PendingLogin {
            state,
            return_to,
            expires_at: expires.and_utc(),
        }))
    }
}
