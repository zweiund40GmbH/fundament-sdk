//! [`ReplicaStore`] for [`MySqlStore`]: one page of records and the cursor in one transaction. The
//! cursor row is locked first, then each record row; a record is written only if its version is
//! higher than the stored one.

use super::MySqlStore;
use crate::consumer::StoreError;
use crate::protocol::feed::should_apply;
use crate::replica::{Record, ReplicaStore};
use crate::sql::db;
use chrono::{NaiveDateTime, Utc};
use sqlx::{MySql, Transaction};

impl MySqlStore {
    /// Writes `r` of `source` if its version is newer; returns whether it was written.
    async fn write_record(
        &self,
        tx: &mut Transaction<'_, MySql>,
        source: &str,
        r: &Record,
        now: NaiveDateTime,
    ) -> Result<bool, StoreError> {
        let stored: Option<u64> = sqlx::query_scalar(self.tables.sql(
            "SELECT `version` FROM {replica} \
             WHERE `source` = ? AND `kind` = ? AND `record_key` = ? FOR UPDATE",
        ))
        .bind(source)
        .bind(&r.kind)
        .bind(&r.key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
        if !should_apply(stored, r.version) {
            return Ok(false);
        }
        let data = r.data.as_ref().map(serde_json::Value::to_string);
        sqlx::query(self.tables.sql(
            "INSERT INTO {replica} (`source`, `kind`, `record_key`, `version`, `deleted`, \
             `external_id`, `data`, `updated_at`) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE `version` = VALUES(`version`), `deleted` = VALUES(`deleted`), \
             `external_id` = VALUES(`external_id`), `data` = VALUES(`data`), \
             `updated_at` = VALUES(`updated_at`)",
        ))
        .bind(source)
        .bind(&r.kind)
        .bind(&r.key)
        .bind(r.version)
        .bind(r.deleted)
        .bind(&r.external_id)
        .bind(data)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
        Ok(true)
    }
}

impl ReplicaStore for MySqlStore {
    async fn apply_records(
        &self,
        source: &str,
        records: &[Record],
        next_cursor: u64,
    ) -> Result<usize, StoreError> {
        let now = Utc::now().naive_utc();
        let mut tx = self.pool.begin().await.map_err(db)?;
        self.lock_cursor(&mut tx, source).await?;
        let mut written = 0;
        for r in records {
            if self.write_record(&mut tx, source, r, now).await? {
                written += 1;
            }
        }
        sqlx::query(
            self.tables
                .sql("UPDATE {cursor} SET `cursor` = ? WHERE `source` = ?"),
        )
        .bind(next_cursor)
        .bind(source)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(written)
    }
}
