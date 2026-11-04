//! MariaDB/MySQL implementation (sqlx) of [`FeedSourceStore`](crate::feed_source::FeedSourceStore) and
//! [`SourceStatusStore`](crate::feed_source::SourceStatusStore): [`MySqlSource`] over the four tables of
//! `schema/mysql-source.sql`, with names chosen by the app ([`SourceTables`]). The library does not
//! create the tables; copy the schema into your migrations.
//!
//! * the **outbox**: [`MySqlSource::write_event`] writes an event in the app's own transaction, together
//!   with the change (the library never opens or commits that transaction)
//! * the **cursor assigner**: under `GET_LOCK` on a held connection, committed events get ascending
//!   cursors in the order of origin; it waits a bounded time for events of a still open transaction and
//!   never skips one (they get a higher cursor in a later round). Each round uses a new connection.
//! * the **retention**: [`MySqlSource::purge`] deletes only superseded events; the newest state of every
//!   record (also a tombstone) stays, so `since=0` is a full initial sync and `gap` is never needed
//! * the **poll log** and the failed attempts per hour, for the status document
//!
//! All SQL uses bind parameters; table names are not input but validated identifiers.
//!
//! **Server settings.** The assigner and the retention update under `READ COMMITTED`; with binary
//! logging this needs `binlog_format` `ROW` or `MIXED` (`STATEMENT` is not supported). Keep transactions
//! that write outbox rows short: a long one delays the feed by at most its own duration.

mod assign;
mod outbox;
mod polls;
mod read;
mod retention;

pub use outbox::{InvalidEvent, OutboxEvent, WriteEventError};
pub use retention::FAILURE_RETENTION_HOURS;

pub use crate::sql::InvalidTableName;

use crate::feed_source::is_valid_type;
use crate::sql::{check_names, check_prefix};
use sqlx::{AssertSqlSafe, MySqlPool};

/// The schema with the default table names (`fundament_outbox`, `fundament_outbox_counter`,
/// `fundament_poll`, `fundament_poll_failure`).
pub const SCHEMA: &str = include_str!("../../schema/mysql-source.sql");

/// A record type is not a valid URN ([`crate::feed_source::is_valid_type`]) or occurs twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid or repeated record type (expected urn:…, at most 128 visible characters, no comma)"
)]
pub struct InvalidType;

/// Names of the four tables and of the assigner's lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTables {
    outbox: String,
    counter: String,
    poll: String,
    failure: String,
    lock: String,
}

impl SourceTables {
    /// The default names with `prefix` in front, e.g. `app_` → `app_fundament_outbox`; the lock is
    /// `app_fundament-cursor:<database>`. The prefix has at most 16 characters of `a-z A-Z 0-9 _`.
    pub fn with_prefix(prefix: &str) -> Result<Self, InvalidTableName> {
        check_prefix(prefix)?;
        let mut t = Self::new(
            &format!("{prefix}fundament_outbox"),
            &format!("{prefix}fundament_outbox_counter"),
            &format!("{prefix}fundament_poll"),
            &format!("{prefix}fundament_poll_failure"),
        )?;
        t.lock = format!("{prefix}fundament-cursor");
        Ok(t)
    }

    /// Freely chosen names (e.g. to avoid a clash with existing tables); the lock is
    /// `<outbox>-cursor:<database>`.
    pub fn new(
        outbox: &str,
        counter: &str,
        poll: &str,
        failure: &str,
    ) -> Result<Self, InvalidTableName> {
        check_names(&[outbox, counter, poll, failure])?;
        Ok(Self {
            outbox: outbox.to_owned(),
            counter: counter.to_owned(),
            poll: poll.to_owned(),
            failure: failure.to_owned(),
            lock: format!("{outbox}-cursor"),
        })
    }

    /// The statements of `schema/mysql-source.sql` with these names, for the app's migration.
    pub fn schema(&self) -> String {
        // The quoted names end with a backtick, so `fundament_outbox` never matches inside
        // `fundament_outbox_counter`.
        self.pairs()
            .iter()
            .fold(SCHEMA.to_owned(), |acc, (default, _, name)| {
                acc.replace(&format!("`{default}`"), &format!("`{name}`"))
            })
    }

    /// `template` with `{outbox}`, `{counter}`, `{poll}` and `{failure}` replaced by the quoted table
    /// names. Safe: the templates are constants of this module and the names validated identifiers.
    fn sql(&self, template: &str) -> AssertSqlSafe<String> {
        AssertSqlSafe(
            self.pairs()
                .iter()
                .fold(template.to_owned(), |acc, (_, placeholder, name)| {
                    acc.replace(placeholder, &format!("`{name}`"))
                }),
        )
    }

    /// Default name, placeholder and chosen name of each table.
    fn pairs(&self) -> [(&'static str, &'static str, &str); 4] {
        [
            ("fundament_outbox", "{outbox}", &self.outbox),
            ("fundament_outbox_counter", "{counter}", &self.counter),
            ("fundament_poll", "{poll}", &self.poll),
            ("fundament_poll_failure", "{failure}", &self.failure),
        ]
    }
}

/// The MariaDB/MySQL store of a feed source. The cursor assigner holds one connection (closed after
/// each round, so its lock and session settings never return to the pool); give the pool at least 2
/// connections.
#[derive(Debug, Clone)]
pub struct MySqlSource {
    pool: MySqlPool,
    tables: SourceTables,
    types: Vec<String>,
}

impl MySqlSource {
    /// A store over `pool` with the tables `tables`, offering the record types `types` (URNs, each
    /// once). Only these types can be written and read.
    pub fn new(
        pool: MySqlPool,
        tables: SourceTables,
        types: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, InvalidType> {
        let mut checked: Vec<String> = Vec::new();
        for t in types.into_iter().map(Into::into) {
            if !is_valid_type(&t) || checked.contains(&t) {
                return Err(InvalidType);
            }
            checked.push(t);
        }
        Ok(Self {
            pool,
            tables,
            types: checked,
        })
    }

    /// The pool.
    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// The table names.
    pub fn tables(&self) -> &SourceTables {
        &self.tables
    }
}

#[cfg(test)]
mod tests;
