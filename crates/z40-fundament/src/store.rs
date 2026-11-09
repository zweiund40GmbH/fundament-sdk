//! The error of an app's storage, shared by every feature that talks to the app's database through a
//! trait (consumer, replica, feed source), and its classification for retry decisions.

use std::fmt;

/// What kind of failure a [`StoreError`] is, so a caller can decide whether to retry without looking
/// at driver types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoreErrorKind {
    /// Transient: a deadlock, a lock wait timeout or a lost/unavailable connection. Running the whole
    /// transaction again may succeed.
    Retryable,
    /// A unique key was violated (e.g. MariaDB/MySQL error 1062): the data conflicts with what is
    /// stored; retrying the same write fails again.
    Conflict,
    /// Anything else (or not classified).
    Other,
}

/// A failure of the app's storage. Wraps the original error and its [`StoreErrorKind`]; it is returned
/// to the app, never sent over the wire. `Display` and `source` are those of the wrapped error.
pub struct StoreError {
    kind: StoreErrorKind,
    error: Box<dyn std::error::Error + Send + Sync>,
}

impl StoreError {
    /// Wraps `error` as [`StoreErrorKind::Other`].
    pub fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_kind(StoreErrorKind::Other, error)
    }

    /// Wraps `error` with a known `kind` (e.g. an app's own store that classifies its driver errors).
    pub fn with_kind(
        kind: StoreErrorKind,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            error: Box::new(error),
        }
    }

    /// The kind of failure. The MariaDB/MySQL implementations of this crate classify their errors;
    /// [`StoreError::new`] is always [`StoreErrorKind::Other`].
    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    /// Whether running the transaction again may succeed ([`StoreErrorKind::Retryable`]).
    pub fn is_retryable(&self) -> bool {
        self.kind == StoreErrorKind::Retryable
    }
}

impl fmt::Debug for StoreError {
    /// The wrapped error's `Debug` (as before the kind existed), followed by the kind.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StoreError")
            .field(&self.error)
            .field(&self.kind)
            .finish()
    }
}

impl fmt::Display for StoreError {
    /// The wrapped error's message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for StoreError {
    /// The wrapped error's source (transparent, like `#[error(transparent)]`).
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_transparency() {
        let e = StoreError::new(std::io::Error::other("disk full"));
        assert_eq!((e.kind(), e.is_retryable()), (StoreErrorKind::Other, false));
        assert_eq!(e.to_string(), "disk full");
        let r = StoreError::with_kind(StoreErrorKind::Retryable, std::io::Error::other("x"));
        assert!(r.is_retryable());
        let c = StoreError::with_kind(StoreErrorKind::Conflict, std::io::Error::other("x"));
        assert!(!c.is_retryable());
        assert!(format!("{c:?}").contains("Conflict"));
    }
}
