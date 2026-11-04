//! Global log setup: JSON lines (one object per event, with the current span such as `request_id`) for
//! production, readable one-line text for local work, filtered by an `EnvFilter` directive
//! (`info,sqlx=warn`). Read from `<PREFIX>_LOG` and `<PREFIX>_LOG_FORMAT`.
//!
//! The filter is taken from `<PREFIX>_LOG`; when that is unset or empty, from `RUST_LOG` (so existing
//! deployments keep working), and otherwise it is [`DEFAULT_FILTER`].
//!
//! ```no_run
//! use z40_fundament_http::telemetry::TelemetryConfig;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // APP_LOG=info,sqlx=warn  APP_LOG_FORMAT=json
//! let config = TelemetryConfig::from_lookup("APP", |k| std::env::var(k).ok())?;
//! config.init()?;
//! # Ok(())
//! # }
//! ```

use tracing_subscriber::EnvFilter;
use z40_fundament::config::{ConfigError, ConfigVar, Lookup};

/// Default filter directive.
pub const DEFAULT_FILTER: &str = "info";

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// One JSON object per line (default: the safe choice for log collectors).
    #[default]
    Json,
    /// Readable text, one line per event (tracing-subscriber's default format).
    Text,
}

/// A filter directive that `EnvFilter` does not accept. The directive is not part of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid log filter directive")]
pub struct InvalidFilter;

/// Format and filter of the global subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// Output format.
    pub format: LogFormat,
    /// `EnvFilter` directive, e.g. `info,sqlx=warn`.
    pub filter: String,
}

impl Default for TelemetryConfig {
    /// JSON with [`DEFAULT_FILTER`].
    fn default() -> Self {
        Self {
            format: LogFormat::Json,
            filter: DEFAULT_FILTER.to_owned(),
        }
    }
}

impl TelemetryConfig {
    /// The variables of [`TelemetryConfig::from_lookup`].
    pub const VARIABLES: &'static [ConfigVar] = &[
        ConfigVar {
            suffix: "LOG",
            default: Some(DEFAULT_FILTER),
            description: "Log filter as `EnvFilter` directive, e.g. `info,sqlx=warn` or `debug`. Unset or \
                          empty: `RUST_LOG`, then `info`. An invalid directive is a configuration error.",
        },
        ConfigVar {
            suffix: "LOG_FORMAT",
            default: Some("json"),
            description: "`json` (one JSON object per line, for log collectors) or `text` (readable text, \
                          one line per event, for local work).",
        },
    ];

    /// Reads `<prefix>_LOG` (unset or empty: `RUST_LOG`, then [`DEFAULT_FILTER`]) and
    /// `<prefix>_LOG_FORMAT` (unset or empty: `json`).
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, &lookup);
        // `RUST_LOG` read through a lookup with prefix `RUST`, so its errors name `RUST_LOG`.
        let rust = Lookup::new("RUST", &lookup);
        let (source, filter) = match l.get("LOG") {
            Some(f) => (&l, f),
            None => (
                &rust,
                rust.get("LOG").unwrap_or_else(|| DEFAULT_FILTER.to_owned()),
            ),
        };
        build_filter(&filter)
            .map_err(|_| source.invalid("LOG", "not a valid EnvFilter directive"))?;
        let format = match l
            .get("LOG_FORMAT")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("json") => LogFormat::Json,
            Some("text") => LogFormat::Text,
            Some(_) => return Err(l.invalid("LOG_FORMAT", "expected json or text")),
        };
        Ok(Self { format, filter })
    }

    /// Installs the global subscriber; see [`init`].
    pub fn init(&self) -> Result<bool, InvalidFilter> {
        init(self.format, &self.filter)
    }
}

/// Installs the global subscriber with `format` and `filter` (also forwarding records of the `log`
/// crate). Idempotent: `Ok(false)` when a subscriber is already installed (a second call, a test
/// harness), which then stays.
pub fn init(format: LogFormat, filter: &str) -> Result<bool, InvalidFilter> {
    let builder = tracing_subscriber::fmt().with_env_filter(build_filter(filter)?);
    let result = match format {
        LogFormat::Json => builder.json().flatten_event(true).try_init(),
        LogFormat::Text => builder.try_init(),
    };
    Ok(result.is_ok())
}

/// The `EnvFilter` of `directive`.
fn build_filter(directive: &str) -> Result<EnvFilter, InvalidFilter> {
    EnvFilter::try_new(directive).map_err(|_| InvalidFilter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn defaults_and_values() {
        assert_eq!(
            TelemetryConfig::from_lookup("APP", env(&[])).unwrap(),
            TelemetryConfig::default()
        );
        let c = TelemetryConfig::from_lookup(
            "APP",
            env(&[("APP_LOG", " debug,sqlx=warn "), ("APP_LOG_FORMAT", "Text")]),
        )
        .unwrap();
        assert_eq!(c.filter, "debug,sqlx=warn");
        assert_eq!(c.format, LogFormat::Text);
        let empty =
            TelemetryConfig::from_lookup("APP", env(&[("APP_LOG", "  "), ("APP_LOG_FORMAT", "")]));
        assert_eq!(empty.unwrap(), TelemetryConfig::default(), "empty = unset");
        let old = env(&[
            ("RUST_LOG", "warn,sqlx=error"),
            ("APP_LOG_FORMAT", "pretty"),
        ]);
        assert!(
            TelemetryConfig::from_lookup("APP", old).is_err(),
            "only json and text"
        );
    }

    #[test]
    fn rust_log_is_the_fallback() {
        let rust_only =
            TelemetryConfig::from_lookup("APP", env(&[("RUST_LOG", " warn,sqlx=error ")]));
        assert_eq!(rust_only.unwrap().filter, "warn,sqlx=error");
        let both = env(&[("APP_LOG", "debug"), ("RUST_LOG", "warn")]);
        assert_eq!(
            TelemetryConfig::from_lookup("APP", both).unwrap().filter,
            "debug",
            "own wins"
        );
        let empty_own = env(&[("APP_LOG", " "), ("RUST_LOG", "warn")]);
        assert_eq!(
            TelemetryConfig::from_lookup("APP", empty_own)
                .unwrap()
                .filter,
            "warn"
        );
        let empty_rust = env(&[("RUST_LOG", "")]);
        assert_eq!(
            TelemetryConfig::from_lookup("APP", empty_rust)
                .unwrap()
                .filter,
            "info"
        );
        let bad =
            TelemetryConfig::from_lookup("APP", env(&[("RUST_LOG", "info,[=x")])).unwrap_err();
        assert_eq!(
            bad.name, "RUST_LOG",
            "the error names the variable that was read"
        );
    }

    #[test]
    fn invalid_values() {
        let bad =
            TelemetryConfig::from_lookup("APP", env(&[("APP_LOG", "info,[=s3cr3t")])).unwrap_err();
        assert_eq!(bad.name, "APP_LOG");
        assert!(!bad.to_string().contains("s3cr3t"));
        let fmt =
            TelemetryConfig::from_lookup("APP", env(&[("APP_LOG_FORMAT", "xml")])).unwrap_err();
        assert_eq!(fmt.name, "APP_LOG_FORMAT");
        assert_eq!(init(LogFormat::Json, "info,[=x"), Err(InvalidFilter));
    }

    #[test]
    fn init_is_idempotent() {
        let first = init(LogFormat::Json, "warn").unwrap();
        assert!(
            !init(LogFormat::Text, "warn").unwrap(),
            "second call keeps the first subscriber"
        );
        let _ = first;
    }
}
