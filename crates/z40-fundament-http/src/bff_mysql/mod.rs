//! MariaDB/MySQL implementation (sqlx) of [`SessionStore`](crate::bff::SessionStore) and
//! [`LoginLimitStore`](crate::bff::LoginLimitStore) (feature `bff-mysql`): [`MySqlSessionStore`] over the
//! session and login tables of `schema/mysql-session.sql`, [`MySqlLoginLimitStore`] over its login
//! limit table, with names chosen by the app ([`SessionTables`]). The library does not create the
//! tables; copy the schema into your migrations.
//!
//! Keys are the SHA-256 hashes of the cookie values (`BINARY(32)`); the cookie values themselves are never
//! written. `sub` and `sid` compare exactly (binary collation). All SQL uses bind parameters; table names
//! are not input but validated identifiers. Run [`MySqlSessionStore::delete_expired`] (and
//! [`MySqlLoginLimitStore::delete_expired`] when it is used) periodically from the app's worker.

mod error;
mod limit;
mod store;

pub use limit::MySqlLoginLimitStore;

use sqlx::{AssertSqlSafe, MySqlPool};

/// The schema with the default table names (`bff_session`, `bff_login`, `bff_login_limit`).
pub const SCHEMA: &str = include_str!("../../schema/mysql-session.sql");

/// Longest table prefix.
const PREFIX_MAX: usize = 16;

/// A table name is not 1–64 characters of `a-z A-Z 0-9 _` (or a prefix longer than 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid table name or prefix (allowed: a-z, A-Z, 0-9, _)")]
pub struct InvalidTableName;

/// `name` as an owned table name if it is 1–64 characters of `a-z A-Z 0-9 _`.
fn table_name(name: &str) -> Result<String, InvalidTableName> {
    if name.is_empty() || !identifier(name, 64) {
        return Err(InvalidTableName);
    }
    Ok(name.to_owned())
}

/// Whether `s` is a safe identifier of at most `max` characters.
fn identifier(s: &str, max: usize) -> bool {
    s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Names of the tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTables {
    session: String,
    login: String,
    login_limit: String,
}

impl Default for SessionTables {
    /// `bff_session`, `bff_login` and `bff_login_limit`.
    fn default() -> Self {
        Self {
            session: "bff_session".to_owned(),
            login: "bff_login".to_owned(),
            login_limit: "bff_login_limit".to_owned(),
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
        )?
        .login_limit_table(&format!("{prefix}bff_login_limit"))
    }

    /// Freely chosen names of the session and login tables (e.g. to keep an app's existing names); the
    /// login limit table keeps `bff_login_limit` (see [`SessionTables::login_limit_table`]).
    pub fn new(session: &str, login: &str) -> Result<Self, InvalidTableName> {
        Ok(Self {
            session: table_name(session)?,
            login: table_name(login)?,
            login_limit: "bff_login_limit".to_owned(),
        })
    }

    /// A freely chosen name of the login limit table ([`MySqlLoginLimitStore`]).
    pub fn login_limit_table(mut self, name: &str) -> Result<Self, InvalidTableName> {
        self.login_limit = table_name(name)?;
        Ok(self)
    }

    /// The statements of `schema/mysql-session.sql` with these names, for the app's migration (index
    /// names keep their defaults; they are per table).
    pub fn schema(&self) -> String {
        SCHEMA
            .replace("`bff_session`", &format!("`{}`", self.session))
            .replace("`bff_login`", &format!("`{}`", self.login))
            .replace("`bff_login_limit`", &format!("`{}`", self.login_limit))
    }

    /// `template` with `{session}`, `{login}` and `{login_limit}` replaced by the quoted names. Safe: the templates are
    /// constants of this module and the names validated identifiers.
    fn sql(&self, template: &str) -> AssertSqlSafe<String> {
        AssertSqlSafe(
            template
                .replace("{session}", &format!("`{}`", self.session))
                .replace("{login}", &format!("`{}`", self.login))
                .replace("{login_limit}", &format!("`{}`", self.login_limit)),
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

    /// The shared login limit over the same pool and the login limit table of these tables.
    pub fn login_limit_store(&self) -> MySqlLoginLimitStore {
        MySqlLoginLimitStore::new(self.pool.clone(), self.tables.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_and_replaced() {
        let t = SessionTables::with_prefix("app_").unwrap();
        let schema = t.schema();
        for name in [
            "`app_bff_session`",
            "`app_bff_login`",
            "`app_bff_login_limit`",
        ] {
            assert!(schema.contains(name), "{name}");
        }
        for name in ["`bff_session`", "`bff_login`", "`bff_login_limit`"] {
            assert!(!schema.contains(name), "{name}");
        }
        assert_eq!(
            t.sql("DELETE FROM {session}; {login}; {login_limit}").0,
            "DELETE FROM `app_bff_session`; `app_bff_login`; `app_bff_login_limit`"
        );
        let own = SessionTables::new("s", "l").unwrap();
        assert_eq!(own.sql("{login_limit}").0, "`bff_login_limit`");
        let own = own.login_limit_table("limits").unwrap();
        assert!(own.schema().contains("`limits`"));
        assert_eq!(own.clone().login_limit_table("a;b"), Err(InvalidTableName));
        assert_eq!(own.login_limit_table(""), Err(InvalidTableName));
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
