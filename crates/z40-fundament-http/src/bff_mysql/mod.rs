//! MariaDB/MySQL implementation (sqlx) of [`SessionStore`](crate::bff::SessionStore) (feature
//! `bff-mysql`): [`MySqlSessionStore`] over the two tables of `schema/mysql-session.sql`, with names
//! chosen by the app ([`SessionTables`]). The library does not create the tables; copy the schema into
//! your migrations.
//!
//! Keys are the SHA-256 hashes of the cookie values (`BINARY(32)`); the cookie values themselves are never
//! written. `sub` and `sid` compare exactly (binary collation). All SQL uses bind parameters; table names
//! are not input but validated identifiers. Run [`MySqlSessionStore::delete_expired`] periodically from
//! the app's worker.

mod store;

use sqlx::{AssertSqlSafe, MySqlPool};

/// The schema with the default table names (`bff_session`, `bff_login`).
pub const SCHEMA: &str = include_str!("../../schema/mysql-session.sql");

/// Longest table prefix.
const PREFIX_MAX: usize = 16;

/// A table name is not 1–64 characters of `a-z A-Z 0-9 _` (or a prefix longer than 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid table name or prefix (allowed: a-z, A-Z, 0-9, _)")]
pub struct InvalidTableName;

/// Whether `s` is a safe identifier of at most `max` characters.
fn identifier(s: &str, max: usize) -> bool {
    s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Names of the two tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTables {
    session: String,
    login: String,
}

impl Default for SessionTables {
    /// `bff_session` and `bff_login`.
    fn default() -> Self {
        Self {
            session: "bff_session".to_owned(),
            login: "bff_login".to_owned(),
        }
    }
}

impl SessionTables {
    /// The default names with `prefix` in front, e.g. `app_` → `app_bff_session`. The prefix has at most
    /// 16 characters of `a-z A-Z 0-9 _`.
    pub fn with_prefix(prefix: &str) -> Result<Self, InvalidTableName> {
        if !identifier(prefix, PREFIX_MAX) {
            return Err(InvalidTableName);
        }
        Self::new(
            &format!("{prefix}bff_session"),
            &format!("{prefix}bff_login"),
        )
    }

    /// Freely chosen names (e.g. to keep an app's existing names).
    pub fn new(session: &str, login: &str) -> Result<Self, InvalidTableName> {
        if [session, login]
            .iter()
            .any(|n| n.is_empty() || !identifier(n, 64))
        {
            return Err(InvalidTableName);
        }
        Ok(Self {
            session: session.to_owned(),
            login: login.to_owned(),
        })
    }

    /// The statements of `schema/mysql-session.sql` with these names, for the app's migration (index
    /// names keep their defaults; they are per table).
    pub fn schema(&self) -> String {
        SCHEMA
            .replace("`bff_session`", &format!("`{}`", self.session))
            .replace("`bff_login`", &format!("`{}`", self.login))
    }

    /// `template` with `{session}` and `{login}` replaced by the quoted names. Safe: the templates are
    /// constants of this module and the names validated identifiers.
    fn sql(&self, template: &str) -> AssertSqlSafe<String> {
        AssertSqlSafe(
            template
                .replace("{session}", &format!("`{}`", self.session))
                .replace("{login}", &format!("`{}`", self.login)),
        )
    }
}

/// The MariaDB/MySQL session store. Cheap to clone (the pool is shared).
#[derive(Debug, Clone)]
pub struct MySqlSessionStore {
    pool: MySqlPool,
    tables: SessionTables,
}

impl MySqlSessionStore {
    /// A store over `pool` with the tables `tables`.
    pub fn new(pool: MySqlPool, tables: SessionTables) -> Self {
        Self { pool, tables }
    }

    /// The pool.
    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// The table names.
    pub fn tables(&self) -> &SessionTables {
        &self.tables
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_and_replaced() {
        let t = SessionTables::with_prefix("app_").unwrap();
        let schema = t.schema();
        assert!(schema.contains("`app_bff_session`") && schema.contains("`app_bff_login`"));
        assert!(!schema.contains("`bff_session`") && !schema.contains("`bff_login`"));
        assert_eq!(
            t.sql("DELETE FROM {session}; {login}").0,
            "DELETE FROM `app_bff_session`; `app_bff_login`"
        );
        assert_eq!(SessionTables::default().schema(), SCHEMA);
        for bad in ["a-b", "a`b", "a b", &"a".repeat(17)] {
            assert_eq!(
                SessionTables::with_prefix(bad),
                Err(InvalidTableName),
                "{bad}"
            );
        }
        assert_eq!(SessionTables::new("", "x"), Err(InvalidTableName));
        assert_eq!(SessionTables::new("x", "y;z"), Err(InvalidTableName));
        assert!(SessionTables::new("sessions", "logins").is_ok());
    }
}
