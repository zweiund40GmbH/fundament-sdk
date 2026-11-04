//! [`CursorStore`] for [`MySqlStore`]: cursor and health per source in the cursor table, and the events
//! on a change of the result in the event table.

use super::{EVENT_RETENTION_DAYS, MySqlStore};
use crate::consumer::{CursorState, CursorStore, FetchError, StoreError};
use crate::health::{RunResult, SourceHealth, transition};
use crate::sql::db;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use sqlx::{MySql, Transaction};

/// A row of the cursor table.
type Row = (
    u64,
    Option<NaiveDateTime>,
    Option<NaiveDateTime>,
    Option<String>,
    Option<String>,
);

impl MySqlStore {
    /// Creates the cursor row of `source` if missing, locks it and returns the stored last result.
    /// Every writer of a source goes through here first, so concurrent transactions queue on the row.
    pub(super) async fn lock_cursor(
        &self,
        tx: &mut Transaction<'_, MySql>,
        source: &str,
    ) -> Result<Option<RunResult>, StoreError> {
        sqlx::query(
            self.tables
                .sql("INSERT IGNORE INTO {cursor} (`source`, `cursor`) VALUES (?, 0)"),
        )
        .bind(source)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
        let last: Option<String> = sqlx::query_scalar(
            self.tables
                .sql("SELECT `last_result` FROM {cursor} WHERE `source` = ? FOR UPDATE"),
        )
        .bind(source)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
        Ok(last.as_deref().map(RunResult::from_code))
    }
}

impl CursorStore for MySqlStore {
    async fn load(&self, source: &str) -> Result<Option<CursorState>, StoreError> {
        let row: Option<Row> = sqlx::query_as(self.tables.sql(
            "SELECT `cursor`, `last_run`, `last_success`, `last_result`, `text` \
             FROM {cursor} WHERE `source` = ?",
        ))
        .bind(source)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.map(
            |(cursor, last_run, last_success, last_result, text)| CursorState {
                cursor,
                health: SourceHealth {
                    last_run: last_run.map(|t| t.and_utc()),
                    last_success: last_success.map(|t| t.and_utc()),
                    last_result: last_result.as_deref().map(RunResult::from_code),
                    text,
                },
            },
        ))
    }

    async fn reset(&self, source: &str) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        self.lock_cursor(&mut tx, source).await?;
        sqlx::query(
            self.tables
                .sql("UPDATE {cursor} SET `cursor` = 0 WHERE `source` = ?"),
        )
        .bind(source)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    async fn finish_run(
        &self,
        source: &str,
        error: Option<FetchError>,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let now = at.naive_utc();
        let text = error.map(FetchError::text);
        let result = if error.is_some() {
            RunResult::Failed
        } else {
            RunResult::Ok
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        let previous = self.lock_cursor(&mut tx, source).await?;
        sqlx::query(self.tables.sql(
            "UPDATE {cursor} SET `last_run` = ?, `last_result` = ?, `text` = ?, \
             `last_success` = IF(? = 'ok', ?, `last_success`) WHERE `source` = ?",
        ))
        .bind(now)
        .bind(result.code())
        .bind(&text)
        .bind(result.code())
        .bind(now)
        .bind(source)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if let Some((level, event_text)) = transition(source, previous, text.as_deref()) {
            sqlx::query(
                self.tables.sql(
                    "INSERT INTO {event} (`time`, `kind`, `level`, `text`) VALUES (?, ?, ?, ?)",
                ),
            )
            .bind(now)
            .bind(&self.event_kind)
            .bind(level.code())
            .bind(event_text)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        sqlx::query(self.tables.sql("DELETE FROM {event} WHERE `time` < ?"))
            .bind(now - Duration::days(EVENT_RETENTION_DAYS))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)
    }
}
