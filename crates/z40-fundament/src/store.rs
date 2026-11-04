//! The error of an app's storage, shared by every feature that talks to the app's database through a
//! trait (consumer, replica, feed source).

/// A failure of the app's storage. Wraps the original error; it is returned to the app, never sent
/// over the wire.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct StoreError(Box<dyn std::error::Error + Send + Sync>);

impl StoreError {
    /// Wraps `error`.
    pub fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}
