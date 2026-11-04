//! Global log setup: JSON lines (one object per event, with the current span such as `request_id`) for
//! production, readable multi-line text for local work, filtered by an `EnvFilter` directive
//! (`info,sqlx=warn`). Read from `<PREFIX>_LOG` and `<PREFIX>_LOG_FORMAT`.
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
    /// Readable multi-line text for a terminal.
    Pretty,
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
            description: "Log filter as `EnvFilter` directive, e.g. `info,sqlx=warn` or `debug`. An invalid \
                          directive is a configuration error.",
        },
        ConfigVar {
            suffix: "LOG_FORMAT",
            default: Some("json"),
            description: "`json` (one JSON object per line, for log collectors) or `pretty` (readable text \
                          for local work).",
        },
    ];

    /// Reads `<prefix>_LOG` and `<prefix>_LOG_FORMAT`; unset or empty values take the defaults.
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        let filter = l.get("LOG").unwrap_or_else(|| DEFAULT_FILTER.to_owned());
        build_filter(&filter).map_err(|_| l.invalid("LOG", "not a valid EnvFilter directive"))?;
        let format = match l
            .get("LOG_FORMAT")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("json") => LogFormat::Json,
            Some("pretty") => LogFormat::Pretty,
            Some(_) => return Err(l.invalid("LOG_FORMAT", "expected json or pretty")),
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
        LogFormat::Pretty => builder.pretty().try_init(),
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
            env(&[
                ("APP_LOG", " debug,sqlx=warn "),
                ("APP_LOG_FORMAT", "Pretty"),
            ]),
        )
        .unwrap();
        assert_eq!(c.filter, "debug,sqlx=warn");
        assert_eq!(c.format, LogFormat::Pretty);
        let empty =
            TelemetryConfig::from_lookup("APP", env(&[("APP_LOG", "  "), ("APP_LOG_FORMAT", "")]));
        assert_eq!(empty.unwrap(), TelemetryConfig::default(), "empty = unset");
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
            !init(LogFormat::Pretty, "warn").unwrap(),
            "second call keeps the first subscriber"
        );
        let _ = first;
    }
}
