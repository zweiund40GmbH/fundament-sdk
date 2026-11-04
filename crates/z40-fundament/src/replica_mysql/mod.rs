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

pub use crate::sql::InvalidTableName;

use crate::replica::{InvalidKind, check_kind};
use crate::sql::{check_names, check_prefix};
use sqlx::{AssertSqlSafe, MySqlPool};

/// Events are kept this many days.
pub const EVENT_RETENTION_DAYS: i64 = 30;
/// Default `kind` of the events written on a change of a source.
pub const DEFAULT_EVENT_KIND: &str = "feed";
/// The schema with the default table names (`fundament_replica`, `fundament_cursor`, `fundament_event`).
pub const SCHEMA: &str = include_str!("../../schema/mysql.sql");

/// Names of the three tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tables {
    replica: String,
    cursor: String,
    event: String,
}

impl Tables {
    /// The default names with `prefix` in front, e.g. `app_` → `app_fundament_replica`.
    /// The prefix has at most 16 characters of `a-z A-Z 0-9 _`.
    pub fn with_prefix(prefix: &str) -> Result<Self, InvalidTableName> {
        check_prefix(prefix)?;
        Self::new(
            &format!("{prefix}fundament_replica"),
            &format!("{prefix}fundament_cursor"),
            &format!("{prefix}fundament_event"),
        )
    }

    /// Freely chosen names (e.g. to avoid a clash with existing tables).
    pub fn new(replica: &str, cursor: &str, event: &str) -> Result<Self, InvalidTableName> {
        check_names(&[replica, cursor, event])?;
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

/// The MariaDB/MySQL store of a consumer and its replica. A run holds one connection for the lock and
/// uses a second one for each transaction, so the pool needs **at least 2** connections
/// (`max_connections`); with one it waits forever.
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

    /// The `kind` of the events written on a change of a source (default [`DEFAULT_EVENT_KIND`],
    /// 1 to 32 characters).
    pub fn event_kind(mut self, kind: impl Into<String>) -> Result<Self, InvalidKind> {
        let kind = kind.into();
        check_kind(&kind)?;
        self.event_kind = kind;
        Ok(self)
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

    #[tokio::test]
    async fn event_kind_length() {
        let pool = MySqlPool::connect_lazy("mysql://x@127.0.0.1:1/x").unwrap();
        let s = MySqlStore::new(pool, Tables::with_prefix("").unwrap());
        assert!(s.clone().event_kind("bezug").is_ok());
        assert_eq!(s.clone().event_kind("").unwrap_err(), InvalidKind);
        assert!(s.event_kind("k".repeat(33)).is_err());
    }
}
