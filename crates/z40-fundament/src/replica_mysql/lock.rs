//! The run lock: `GET_LOCK` on a pooled connection that is held for the whole run. The lock belongs to
//! that connection, so it is released explicitly before the connection goes back to the pool (and by
//! the server if the connection dies). The name is qualified with the database, since locks are
//! server-wide.

use super::{MySqlStore, db};
use crate::consumer::{RunLock, StoreError};
use sqlx::MySql;
use sqlx::pool::PoolConnection;

/// Takes the lock of `source` without waiting. The name is `fundament_feed:<source>:<database>`, cut to
/// the 64 characters MariaDB allows (the source is validated, the database name comes from the server).
const GET: &str = "SELECT GET_LOCK(LEFT(CONCAT('fundament_feed:', ?, ':', DATABASE()), 64), 0)";
/// Releases the lock of `source` (same name as [`GET`]).
const RELEASE: &str =
    "SELECT RELEASE_LOCK(LEFT(CONCAT('fundament_feed:', ?, ':', DATABASE()), 64))";

/// A held lock: the connection and the source.
#[derive(Debug)]
pub struct MySqlLockGuard {
    conn: PoolConnection<MySql>,
    source: String,
}

impl RunLock for MySqlStore {
    type Guard = MySqlLockGuard;

    async fn try_lock(&self, source: &str) -> Result<Option<MySqlLockGuard>, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(db)?;
        let got: Option<i64> = sqlx::query_scalar(GET)
            .bind(source)
            .fetch_one(&mut *conn)
            .await
            .map_err(db)?;
        Ok((got == Some(1)).then(|| MySqlLockGuard {
            conn,
            source: source.to_owned(),
        }))
    }

    async fn unlock(&self, mut guard: MySqlLockGuard) -> Result<(), StoreError> {
        sqlx::query(RELEASE)
            .bind(&guard.source)
            .execute(&mut *guard.conn)
            .await
            .map_err(db)?;
        Ok(())
    }
}
