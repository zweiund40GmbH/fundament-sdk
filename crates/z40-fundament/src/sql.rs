//! What the MariaDB/MySQL implementations share: validated table names and the classification of
//! database errors ([`StoreErrorKind`]). Table names cannot be bind parameters, so they are checked
//! identifiers and only ever quoted into constant statement templates.

use crate::{StoreError, StoreErrorKind};

/// A table name is not 1–64 characters of `a-z A-Z 0-9 _` (or a prefix longer than 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid table name or prefix (allowed: a-z, A-Z, 0-9, _)")]
pub struct InvalidTableName;

/// Longest table prefix.
pub(crate) const PREFIX_MAX: usize = 16;

/// Whether `s` is a safe identifier of at most `max` characters (`a-z A-Z 0-9 _`).
pub(crate) fn identifier(s: &str, max: usize) -> bool {
    s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Checks a table prefix (0–16 characters of `a-z A-Z 0-9 _`).
pub(crate) fn check_prefix(prefix: &str) -> Result<(), InvalidTableName> {
    identifier(prefix, PREFIX_MAX)
        .then_some(())
        .ok_or(InvalidTableName)
}

/// Checks full table names (1–64 characters of `a-z A-Z 0-9 _`).
pub(crate) fn check_names(names: &[&str]) -> Result<(), InvalidTableName> {
    names
        .iter()
        .all(|n| !n.is_empty() && identifier(n, 64))
        .then_some(())
        .ok_or(InvalidTableName)
}

/// MariaDB/MySQL error number of a duplicate key.
pub(crate) const ER_DUP_ENTRY: u16 = 1062;
/// MariaDB/MySQL error number of a lock wait timeout.
const ER_LOCK_WAIT_TIMEOUT: u16 = 1205;
/// MariaDB/MySQL error number of a deadlock.
const ER_LOCK_DEADLOCK: u16 = 1213;

/// The kind of a server error number: deadlock and lock wait timeout are retryable (the transaction
/// was rolled back or may be run again), a duplicate key is a conflict.
fn kind_of_number(number: u16) -> StoreErrorKind {
    match number {
        ER_LOCK_DEADLOCK | ER_LOCK_WAIT_TIMEOUT => StoreErrorKind::Retryable,
        ER_DUP_ENTRY => StoreErrorKind::Conflict,
        _ => StoreErrorKind::Other,
    }
}

/// The MariaDB/MySQL error number of `e`, if the server answered with an error.
pub(crate) fn error_number(e: &sqlx::Error) -> Option<u16> {
    e.as_database_error()
        .and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .map(|m| m.number())
}

/// The kind of a database error: server errors by number; I/O errors (lost or refused connection),
/// a pool timeout and a crashed connection worker are retryable.
fn kind_of(e: &sqlx::Error) -> StoreErrorKind {
    match e {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::WorkerCrashed => {
            StoreErrorKind::Retryable
        }
        _ => error_number(e).map_or(StoreErrorKind::Other, kind_of_number),
    }
}

/// Wraps a database error with its [`StoreErrorKind`].
pub(crate) fn db(e: sqlx::Error) -> StoreError {
    StoreError::with_kind(kind_of(&e), e)
}

#[cfg(test)]
#[path = "sql_tests.rs"]
mod tests;
