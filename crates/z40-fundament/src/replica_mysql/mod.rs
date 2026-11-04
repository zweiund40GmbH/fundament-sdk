//! MariaDB/MySQL implementations (sqlx) of [`RunLock`](crate::consumer::RunLock),
//! [`CursorStore`](crate::consumer::CursorStore) and [`ReplicaStore`](crate::replica::ReplicaStore):
//! [`MySqlStore`] over the three tables of `schema/mysql.sql`, with names chosen by the app
//! ([`Tables`]). The library does not create the tables; copy the schema into your migrations.
//!
//! * `GET_LOCK` on a connection held for the whole run (named per source and database)
//! * page and cursor in one transaction; the cursor row is locked first, so concurrent writers queue
//! * events on a change between ok and failing, purged after [`EVENT_RETENTION_DAYS`]
//!
//! All SQL uses bind parameters; table names are not input but validated identifiers.

mod apply;
mod cursor;
mod lock;
mod read;

pub use lock::MySqlLockGuard;
pub use read::{StoredEvent, StoredRecord};

use sqlx::{AssertSqlSafe, MySqlPool};

/// Events are kept this many days.
pub const EVENT_RETENTION_DAYS: i64 = 30;
/// Default `kind` of the events written on a change of a source.
pub const DEFAULT_EVENT_KIND: &str = "feed";
/// The schema with the default table names (`fundament_replica`, `fundament_cursor`, `fundament_event`).
pub const SCHEMA: &str = include_str!("../../schema/mysql.sql");

/// A table name is not 1–64 characters of `a-z A-Z 0-9 _` (or a prefix longer than 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid table name or prefix (allowed: a-z, A-Z, 0-9, _)")]
pub struct InvalidTableName;

/// Names of the three tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tables {
    replica: String,
    cursor: String,
    event: String,
}

/// Whether `s` is a safe identifier of at most `max` characters (`a-z A-Z 0-9 _`).
fn identifier(s: &str, max: usize) -> bool {
    s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Tables {
    /// The default names with `prefix` in front, e.g. `app_` → `app_fundament_replica`.
    /// The prefix has at most 16 characters of `a-z A-Z 0-9 _`.
    pub fn with_prefix(prefix: &str) -> Result<Self, InvalidTableName> {
        if !identifier(prefix, 16) {
            return Err(InvalidTableName);
        }
        Self::new(
            &format!("{prefix}fundament_replica"),
            &format!("{prefix}fundament_cursor"),
            &format!("{prefix}fundament_event"),
        )
    }

    /// Freely chosen names (e.g. to avoid a clash with existing tables).
    pub fn new(replica: &str, cursor: &str, event: &str) -> Result<Self, InvalidTableName> {
        for name in [replica, cursor, event] {
            if name.is_empty() || !identifier(name, 64) {
                return Err(InvalidTableName);
            }
        }
        Ok(Self {
            replica: replica.to_owned(),
            cursor: cursor.to_owned(),
            event: event.to_owned(),
        })
    }

    /// The `CREATE TABLE` statements of `schema/mysql.sql` with these names, for the app's migration.
    pub fn schema(&self) -> String {
        SCHEMA
            .replace("`fundament_replica`", &format!("`{}`", self.replica))
            .replace("`fundament_cursor`", &format!("`{}`", self.cursor))
            .replace("`fundament_event`", &format!("`{}`", self.event))
    }

    /// `template` with `{replica}`, `{cursor}` and `{event}` replaced by the quoted table names. Safe:
    /// the templates are constants of this module and the names are validated identifiers.
    fn sql(&self, template: &str) -> AssertSqlSafe<String> {
        AssertSqlSafe(
            template
                .replace("{replica}", &format!("`{}`", self.replica))
                .replace("{cursor}", &format!("`{}`", self.cursor))
                .replace("{event}", &format!("`{}`", self.event)),
        )
    }
}

/// The MariaDB/MySQL store of a consumer and its replica.
#[derive(Debug, Clone)]
pub struct MySqlStore {
    pool: MySqlPool,
    tables: Tables,
    event_kind: String,
}

impl MySqlStore {
    /// A store over `pool` with the tables `tables`.
    pub fn new(pool: MySqlPool, tables: Tables) -> Self {
        Self {
            pool,
            tables,
            event_kind: DEFAULT_EVENT_KIND.to_owned(),
        }
    }

    /// The `kind` of the events written on a change of a source (default [`DEFAULT_EVENT_KIND`]).
    pub fn event_kind(mut self, kind: impl Into<String>) -> Self {
        self.event_kind = kind.into();
        self
    }

    /// The pool.
    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// The table names.
    pub fn tables(&self) -> &Tables {
        &self.tables
    }
}

/// Wraps a database error.
fn db(e: sqlx::Error) -> crate::consumer::StoreError {
    crate::consumer::StoreError::new(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names() {
        let t = Tables::with_prefix("app_").unwrap();
        assert_eq!(t.replica, "app_fundament_replica");
        let s = t.schema();
        assert!(s.contains("CREATE TABLE IF NOT EXISTS `app_fundament_cursor`"));
        assert!(!s.contains("`fundament_"));
        assert_eq!(
            t.sql("SELECT * FROM {event}").0,
            "SELECT * FROM `app_fundament_event`"
        );
        assert!(Tables::with_prefix("").is_ok());
        for bad in ["a-b", "x`; DROP", "toolongprefix_____"] {
            assert_eq!(Tables::with_prefix(bad), Err(InvalidTableName), "{bad}");
        }
        assert!(Tables::new("a", "", "c").is_err());
        assert!(Tables::new("r", "c", "e").unwrap().schema().contains("`e`"));
    }
}
