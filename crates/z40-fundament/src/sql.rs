//! What the MariaDB/MySQL implementations share: validated table names. Table names cannot be bind
//! parameters, so they are checked identifiers and only ever quoted into constant statement templates.

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

/// Wraps a database error.
pub(crate) fn db(e: sqlx::Error) -> crate::StoreError {
    crate::StoreError::new(e)
}
