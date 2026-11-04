//! Settings of a feed consumer (`<PREFIX>_URL`, `<PREFIX>_KEY`, `<PREFIX>_INTERVAL_SECS`) and the
//! validation of the source origin. Configuration only accepts `https://` origins; plain http exists
//! only behind [`super::FeedConsumerBuilder::plain_http_for_tests`], which config cannot reach.

use crate::config::{ConfigError, ConfigErrorKind, ConfigVar, Lookup};
use crate::credentials::BearerKey;
use crate::http_client::https_origin;
use std::ops::RangeInclusive;
use std::time::Duration;

/// Default of `<PREFIX>_INTERVAL_SECS`.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;
/// Allowed range of `<PREFIX>_INTERVAL_SECS`.
pub const INTERVAL_RANGE: RangeInclusive<u64> = 30..=3600;

/// Settings of one consumed feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedConsumerConfig {
    /// `<PREFIX>_URL`: https origin of the source, validated. `None` = feed off.
    pub url: Option<String>,
    /// `<PREFIX>_KEY`: bearer key of this app at the source. `None` = feed off.
    pub key: Option<BearerKey>,
    /// `<PREFIX>_INTERVAL_SECS`: seconds between runs.
    pub interval_secs: u64,
}

impl Default for FeedConsumerConfig {
    /// The off state: no URL, no key, the default interval.
    fn default() -> Self {
        Self {
            url: None,
            key: None,
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }
}

impl FeedConsumerConfig {
    /// The off state (same as [`Default`]), e.g. for apps that do not consume this feed.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// The variables read by [`FeedConsumerConfig::from_lookup`] (documented in `docs/configuration.md`).
    pub const VARIABLES: &'static [ConfigVar] = &[
        ConfigVar {
            suffix: "URL",
            default: None,
            description: "Origin of the source app, `https://host[:port]` without path. Unset: the feed \
                is off. Processes that only report the status may get the URL without the key.",
        },
        ConfigVar {
            suffix: "KEY",
            default: None,
            description: "Feed key of this app at the source (sent as `Authorization: Bearer`). Unset: \
                the feed is off. Secret: only for the process that runs the consumer.",
        },
        ConfigVar {
            suffix: "INTERVAL_SECS",
            default: Some("60"),
            description: "Seconds between runs, 30–3600. Used by the app's scheduler (`slot_for`).",
        },
    ];

    /// Reads the settings with the variable prefix `prefix` (e.g. `CRM_FEED` → `CRM_FEED_URL`).
    /// Only `https://` origins are accepted.
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        let url = l
            .get("URL")
            .map(|raw| {
                https_origin(&raw).ok_or_else(|| {
                    l.error(
                        "URL",
                        ConfigErrorKind::InvalidUrl,
                        "expected an https origin like https://host",
                    )
                })
            })
            .transpose()?;
        let interval_secs = l.number("INTERVAL_SECS", DEFAULT_INTERVAL_SECS, INTERVAL_RANGE)?;
        let key = l
            .get("KEY")
            .map(|k| {
                BearerKey::new(k)
                    .map_err(|e| l.error("KEY", ConfigErrorKind::InvalidKey(e), e.to_string()))
            })
            .transpose()?;
        Ok(Self {
            url,
            key,
            interval_secs,
        })
    }

    /// Whether the feed is switched on (URL and key set).
    pub fn is_enabled(&self) -> bool {
        self.url.is_some() && self.key.is_some()
    }

    /// The interval between runs.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<FeedConsumerConfig, ConfigError> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        FeedConsumerConfig::from_lookup("CRM_FEED", move |n| {
            owned.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn defaults_and_switch() {
        let off = cfg(&[]).unwrap();
        assert!(!off.is_enabled() && off.url.is_none());
        assert_eq!(off.interval(), Duration::from_secs(60));
        let url_only = cfg(&[("CRM_FEED_URL", "https://crm.example.com/")]).unwrap();
        assert_eq!(url_only.url.as_deref(), Some("https://crm.example.com"));
        assert!(!url_only.is_enabled());
        let on = cfg(&[
            ("CRM_FEED_URL", "https://crm.example.com:8443"),
            ("CRM_FEED_KEY", "s3cr3t"),
            ("CRM_FEED_INTERVAL_SECS", "30"),
        ])
        .unwrap();
        assert!(on.is_enabled());
        assert_eq!(on.url.as_deref(), Some("https://crm.example.com:8443"));
        assert_eq!(on.interval_secs, 30);
        assert!(!format!("{on:?}").contains("s3cr3t"));
    }

    #[test]
    fn disabled_is_the_unset_state() {
        assert_eq!(FeedConsumerConfig::disabled(), cfg(&[]).unwrap());
        assert!(!FeedConsumerConfig::default().is_enabled());
    }

    #[test]
    fn invalid_settings() {
        for (k, v) in [
            ("CRM_FEED_URL", "http://crm.example.com"),
            ("CRM_FEED_URL", "https://crm.example.com/feed"),
            ("CRM_FEED_URL", "https://user:pw@crm.example.com"),
            ("CRM_FEED_URL", "https:///crm.example.com"),
            ("CRM_FEED_URL", "https://crm.example.com?x=1"),
            ("CRM_FEED_INTERVAL_SECS", "29"),
            ("CRM_FEED_INTERVAL_SECS", "3601"),
            ("CRM_FEED_KEY", "two words"),
        ] {
            let err = cfg(&[(k, v)]).unwrap_err();
            assert_eq!(err.name, k);
            let kind = match k {
                "CRM_FEED_URL" => ConfigErrorKind::InvalidUrl,
                "CRM_FEED_KEY" => ConfigErrorKind::InvalidKey(crate::credentials::InvalidKey),
                _ => ConfigErrorKind::OutOfRange { min: 30, max: 3600 },
            };
            assert_eq!(err.kind, kind);
            assert!(!err.to_string().contains(v), "value echoed: {err}");
        }
    }
}
