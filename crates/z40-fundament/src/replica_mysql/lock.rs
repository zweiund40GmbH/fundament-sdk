//! The run lock: `GET_LOCK` on a pooled connection that is held for the whole run. The lock belongs to
//! that connection's session. A connection that took the lock is **never returned to the pool**: it is
//! closed when the guard is dropped (`close_on_drop`), so the server releases the lock even when the
//! run is cancelled, panics or `RELEASE_LOCK` fails – otherwise a pooled session would keep the lock (and
//! a later `GET_LOCK` on the same session would nest instead of failing). This costs one reconnect per
//! run. The name is qualified with the database, since locks are server-wide.

use super::MySqlStore;
use crate::consumer::{RunLock, StoreError};
use crate::sql::db;
use sqlx::MySql;
use sqlx::pool::PoolConnection;

/// Takes the lock of `source` without waiting. The name is `fundament_feed:<source>:<database>`, cut to
/// the 64 characters MariaDB allows (the source is validated, the database name comes from the server).
const GET: &str = "SELECT GET_LOCK(LEFT(CONCAT('fundament_feed:', ?, ':', DATABASE()), 64), 0)";
/// Releases the lock of `source` (same name as [`GET`]).
const RELEASE: &str =
    "SELECT RELEASE_LOCK(LEFT(CONCAT('fundament_feed:', ?, ':', DATABASE()), 64))";

/// A held lock: the connection (closed on drop, never pooled again) and the source.
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
        if got != Some(1) {
            // Not taken: the session holds no lock, the connection may go back to the pool.
            return Ok(None);
        }
        conn.close_on_drop();
        Ok(Some(MySqlLockGuard {
            conn,
            source: source.to_owned(),
        }))
    }

    /// Releases the lock explicitly; on error the guard is dropped anyway and closing its connection
    /// ends the session, which releases the lock on the server.
    async fn unlock(&self, mut guard: MySqlLockGuard) -> Result<(), StoreError> {
        let released = sqlx::query(RELEASE)
            .bind(&guard.source)
            .execute(&mut *guard.conn)
            .await;
        drop(guard);
        released.map(|_| ()).map_err(db)
    }
}
