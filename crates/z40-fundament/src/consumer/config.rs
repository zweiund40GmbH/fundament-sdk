//! Settings of a feed consumer (`<PREFIX>_URL`, `<PREFIX>_KEY`, `<PREFIX>_INTERVAL_SECS`) and the
//! validation of the source origin. Configuration only accepts `https://` origins; plain http exists
//! only behind [`super::FeedConsumerBuilder::plain_http_for_tests`], which config cannot reach.

use crate::config::{ConfigError, ConfigVar, Lookup};
use crate::credentials::BearerKey;
use std::ops::RangeInclusive;
use std::time::Duration;

/// Default of `<PREFIX>_INTERVAL_SECS`.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;
/// Allowed range of `<PREFIX>_INTERVAL_SECS`.
pub const INTERVAL_RANGE: RangeInclusive<u64> = 30..=3600;

/// The origin of `raw` (`scheme://host[:port]`, no trailing slash) if it is exactly an origin of
/// `scheme`: no user info, no path (one trailing `/` allowed), no query, no fragment.
fn origin(raw: &str, scheme: &str) -> Option<reqwest::Url> {
    let raw = raw.trim();
    // The URL parser silently repairs forms like `https:///host`; the raw text must already be
    // `scheme://authority` with at most one trailing slash.
    let rest = raw.strip_prefix(scheme)?.strip_prefix("://")?;
    if rest.trim_end_matches('/').contains('/') || rest.ends_with("//") {
        return None;
    }
    reqwest::Url::parse(raw).ok().filter(|u| {
        u.scheme() == scheme
            && u.host_str().is_some_and(|h| !h.is_empty())
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/"
            && u.query().is_none()
            && u.fragment().is_none()
    })
}

/// The `https://` origin of `raw`, serialized without trailing slash.
pub(crate) fn https_origin(raw: &str) -> Option<String> {
    origin(raw, "https").map(|u| u.origin().ascii_serialization())
}

/// The `http://` origin of `raw` if its host is a loopback address (tests only).
pub(crate) fn loopback_http_origin(raw: &str) -> Option<String> {
    let u = origin(raw, "http")?;
    let host = u.host_str()?;
    let ip = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost"
        || ip
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback());
    loopback.then(|| u.origin().ascii_serialization())
}

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

impl FeedConsumerConfig {
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
                https_origin(&raw)
                    .ok_or_else(|| l.invalid("URL", "expected an https origin like https://host"))
            })
            .transpose()?;
        let interval_secs = l.number("INTERVAL_SECS", DEFAULT_INTERVAL_SECS, INTERVAL_RANGE)?;
        let key = l
            .get("KEY")
            .map(|k| BearerKey::new(k).map_err(|e| l.invalid("KEY", e.to_string())))
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
            assert!(!err.to_string().contains(v), "value echoed: {err}");
        }
    }

    #[test]
    fn loopback_only_for_plain_http() {
        assert_eq!(
            loopback_http_origin("http://127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert!(loopback_http_origin("http://localhost:1").is_some());
        assert!(loopback_http_origin("http://[::1]:1").is_some());
        assert!(loopback_http_origin("http://crm.example.com").is_none());
        assert!(loopback_http_origin("https://127.0.0.1").is_none());
        assert!(https_origin("http://127.0.0.1").is_none());
    }
}
