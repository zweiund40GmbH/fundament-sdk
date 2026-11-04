//! Settings of a feed source: the accepted feed keys (`<PREFIX>_FEED_CLIENTS`) and the retention of
//! superseded events (`<PREFIX>_FEED_RETENTION_DAYS`).

use crate::config::{ConfigError, ConfigVar, Lookup};
use crate::credentials::KeyList;
use std::ops::RangeInclusive;
use std::time::Duration;

/// Default of `<PREFIX>_FEED_RETENTION_DAYS` (the protocol's recommendation).
pub const DEFAULT_RETENTION_DAYS: u64 = 30;
/// Allowed range of `<PREFIX>_FEED_RETENTION_DAYS`.
pub const RETENTION_DAYS_RANGE: RangeInclusive<u64> = 7..=3650;

/// Settings of a feed source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSourceConfig {
    /// Accepted feed keys (`<PREFIX>_FEED_CLIENTS`); empty = every feed request gets `401`.
    pub clients: KeyList,
    /// Days after its `occurred_at` a superseded event is deleted (`<PREFIX>_FEED_RETENTION_DAYS`).
    pub retention_days: u64,
}

impl FeedSourceConfig {
    /// The variables read by [`FeedSourceConfig::from_lookup`] (documented in `docs/configuration.md`).
    pub const VARIABLES: &'static [ConfigVar] = &[
        ConfigVar {
            suffix: "FEED_CLIENTS",
            default: None,
            description: "Accepted feed keys as comma-separated `name=sha256hex` entries (only the SHA-256 \
                of a key, never the key); the name identifies the consumer in the status document. Unset \
                or empty: every feed request gets `401`. Keep it separate from status keys.",
        },
        ConfigVar {
            suffix: "FEED_RETENTION_DAYS",
            default: Some("30"),
            description: "Retention of superseded events, 7–3650 days: an event is deleted once a newer \
                version of the same record exists and its own `occurred_at` is older than this. The newest \
                state of every record (also a tombstone) is always kept.",
        },
    ];

    /// Reads the settings with the variable prefix `prefix` (e.g. `APP` → `APP_FEED_CLIENTS`).
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let clients = KeyList::from_lookup(prefix, "FEED_CLIENTS", &lookup)?;
        let retention_days = Lookup::new(prefix, lookup).number(
            "FEED_RETENTION_DAYS",
            DEFAULT_RETENTION_DAYS,
            RETENTION_DAYS_RANGE,
        )?;
        Ok(Self {
            clients,
            retention_days,
        })
    }

    /// The retention as a duration (for `purge`).
    pub fn retention(&self) -> Duration {
        Duration::from_secs(self.retention_days.saturating_mul(24 * 3600))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::key_entry;

    fn cfg(pairs: Vec<(String, String)>) -> Result<FeedSourceConfig, ConfigError> {
        FeedSourceConfig::from_lookup("APP", move |n| {
            pairs.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn defaults_and_values() {
        let d = cfg(vec![]).unwrap();
        assert!(d.clients.is_empty());
        assert_eq!(d.retention_days, 30);
        assert_eq!(d.retention(), Duration::from_secs(30 * 86_400));
        let entry = key_entry("crm", "k1").unwrap();
        let c = cfg(vec![
            ("APP_FEED_CLIENTS".into(), entry),
            ("APP_FEED_RETENTION_DAYS".into(), "7".into()),
        ])
        .unwrap();
        assert_eq!(c.clients.verify("k1"), Some("crm"));
        assert_eq!(c.retention_days, 7);
    }

    #[test]
    fn invalid_settings_name_the_variable() {
        for (k, v) in [
            ("APP_FEED_RETENTION_DAYS", "6"),
            ("APP_FEED_RETENTION_DAYS", "3651"),
            ("APP_FEED_CLIENTS", "crm=s3cr3t-key"),
        ] {
            let err = cfg(vec![(k.into(), v.into())]).unwrap_err();
            assert_eq!(err.name, k);
            assert!(!err.to_string().contains("s3cr3t"), "value echoed: {err}");
        }
    }
}
