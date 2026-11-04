//! The status document of an app (`GET /fundament/v1/status`, `docs/protocol.md` §3): types with the
//! exact wire names, a builder that derives the overall level, and an axum router that checks the
//! status key, brakes guessers and answers within the deadline.
//!
//! * [`document`] – the wire types
//! * [`builder`] – [`StatusBuilder`]
//! * [`handler`] – [`StatusEndpoint`] and the [`StatusProvider`] trait the app implements

pub mod builder;
pub mod document;
pub mod handler;

pub use builder::{BACKLOG_CAP, MAX_EVENTS, StatusBuilder, events_per_minute};
pub use document::{Note, Offer, Poller, Source, StatusDocument, StatusEvent};
pub use handler::{ClientIp, FailedAttemptHook, StatusEndpoint, StatusProvider};

use crate::config::{ConfigError, ConfigVar};
use crate::credentials::KeyList;

/// Settings of the status endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusConfig {
    /// Accepted status keys (`<PREFIX>_STATUS_CLIENTS`); empty = every request gets `401`.
    pub clients: KeyList,
}

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

#[cfg(test)]
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
