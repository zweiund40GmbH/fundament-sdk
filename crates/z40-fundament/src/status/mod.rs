//! The status document of an app (`GET /fundament/v1/status`, `docs/protocol.md` §3): types with the
//! exact wire names, and both sides of the endpoint.
//!
//! * [`document`] – the wire types (re-exported from [`crate::protocol::status`], which needs no feature)
//! * `status`: [`builder`] – [`StatusBuilder`] derives the overall level; [`handler`] –
//!   [`StatusEndpoint`] (axum) checks the status key, brakes guessers and answers within the deadline,
//!   with the [`StatusProvider`] trait the app implements
//! * `status-client`: [`StatusClient`] reads the document of another app, [`StatusKeys`] holds one
//!   status key per target app, [`StatusPoller`] reads many apps over one HTTP client (with
//!   [`StatusPoller::poll_all`] for bounded concurrency; [`poll_all`] builds a poller per call)

#[cfg(feature = "status")]
pub mod builder;
#[cfg(feature = "status-client")]
mod client;
pub mod document;
#[cfg(feature = "status")]
pub mod handler;
#[cfg(feature = "status-client")]
mod keys;
#[cfg(feature = "status-client")]
mod poll;

pub use crate::protocol::status::InvalidDocument;
#[cfg(feature = "status")]
pub use builder::{BACKLOG_CAP, MAX_EVENTS, StatusBuilder, events_per_minute};
#[cfg(feature = "status-client")]
pub use client::{
    FETCH_BODY_MAX, FETCH_DEADLINE, StatusClient, StatusClientError, StatusFetchError, StatusTarget,
};
pub use document::{Note, Offer, Poller, Source, StatusDocument, StatusEvent};
#[cfg(feature = "status")]
pub use handler::{ClientIp, FailedAttemptHook, StatusEndpoint, StatusProvider};
#[cfg(feature = "status-client")]
pub use keys::{StatusKeys, StatusKeysError};
#[cfg(feature = "status-client")]
pub use poll::{PollResult, StatusPoller, poll_all};

#[cfg(feature = "status")]
use crate::config::{ConfigError, ConfigVar};
#[cfg(feature = "status")]
use crate::credentials::KeyList;

/// Settings of the status endpoint.
#[cfg(feature = "status")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusConfig {
    /// Accepted status keys (`<PREFIX>_STATUS_CLIENTS`); empty = every request gets `401`.
    pub clients: KeyList,
}

#[cfg(feature = "status")]
impl StatusConfig {
    /// The variables read by [`StatusConfig::from_lookup`] (documented in `docs/configuration.md`).
    pub const VARIABLES: &'static [ConfigVar] = &[ConfigVar {
        suffix: "STATUS_CLIENTS",
        default: None,
        description: "Accepted status keys as comma-separated `name=sha256hex` entries (only the SHA-256 \
            of a key, never the key). Unset or empty: every status request gets `401`. Keep it separate \
            from feed keys.",
    }];

    /// Reads the settings with the variable prefix `prefix` (e.g. `APP` → `APP_STATUS_CLIENTS`).
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            clients: KeyList::from_lookup(prefix, "STATUS_CLIENTS", lookup)?,
        })
    }
}

#[cfg(all(test, feature = "status"))]
mod tests {
    use super::*;
    use crate::credentials::key_entry;

    #[test]
    fn config_from_lookup() {
        let entry = key_entry("monitor", "k").unwrap();
        let on = StatusConfig::from_lookup("APP", |n| {
            (n == "APP_STATUS_CLIENTS").then(|| entry.clone())
        })
        .unwrap();
        assert_eq!(on.clients.verify("k"), Some("monitor"));
        assert!(
            StatusConfig::from_lookup("APP", |_| None)
                .unwrap()
                .clients
                .is_empty()
        );
        assert!(StatusConfig::from_lookup("APP", |_| Some("bad".into())).is_err());
    }
}
