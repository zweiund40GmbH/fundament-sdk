//! Classification of database errors for the stores of this module, the same as in `z40-fundament`:
//! server errors by number ([`StoreErrorKind::from_mysql_code`]: deadlock and lock wait timeout are
//! retryable, a duplicate key is a conflict); a lost or refused connection, a pool timeout and a
//! crashed connection worker are retryable. Lets the app retry a failed login or session write.

use z40_fundament::{StoreError, StoreErrorKind};

/// The kind of a database error. Keep in sync with `kind_of` in `z40-fundament` (`src/sql.rs`), which
/// cannot be shared without putting sqlx into the public API of the core crate.
fn kind_of(e: &sqlx::Error) -> StoreErrorKind {
    match e {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::WorkerCrashed => {
            StoreErrorKind::Retryable
        }
        _ => e
            .as_database_error()
            .and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
            .map_or(StoreErrorKind::Other, |m| {
                StoreErrorKind::from_mysql_code(m.number())
            }),
    }
}

/// Wraps a database error with its [`StoreErrorKind`].
pub(super) fn db(e: sqlx::Error) -> StoreError {
    StoreError::with_kind(kind_of(&e), e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_errors_are_retryable() {
        assert_eq!(
            db(sqlx::Error::PoolTimedOut).kind(),
            StoreErrorKind::Retryable
        );
        assert_eq!(
            db(sqlx::Error::WorkerCrashed).kind(),
            StoreErrorKind::Retryable
        );
        let io = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        assert_eq!(db(sqlx::Error::Io(io)).kind(), StoreErrorKind::Retryable);
    }

    #[test]
    fn other_errors_are_other() {
        assert_eq!(db(sqlx::Error::RowNotFound).kind(), StoreErrorKind::Other);
        assert_eq!(db(sqlx::Error::PoolClosed).kind(), StoreErrorKind::Other);
    }
}
