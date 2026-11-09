//! Global log setup: JSON lines (one object per event, with the current span such as `request_id`) for
//! production, readable one-line text for local work, filtered by an `EnvFilter` directive
//! (`info,sqlx=warn`). Read from `<PREFIX>_LOG` and `<PREFIX>_LOG_FORMAT`.
//!
//! The filter is taken from `<PREFIX>_LOG`; when that is unset or empty, from `RUST_LOG` (so existing
//! deployments keep working), and otherwise it is [`DEFAULT_FILTER`] – or the app's own default with
//! [`TelemetryConfig::from_lookup_with`], which also sets the default format (e.g. `text` in development).
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

/// The app's defaults for [`TelemetryConfig::from_lookup_with`], used when the variables are unset or
/// empty. `Default`: [`DEFAULT_FILTER`] and JSON (what [`TelemetryConfig::from_lookup`] uses).
///
/// ```
/// use z40_fundament_http::telemetry::{Defaults, LogFormat, TelemetryConfig};
///
/// # fn main() -> Result<(), z40_fundament::config::ConfigError> {
/// let development = true;
/// let defaults = Defaults {
///     filter: "info,sqlx=warn".into(),
///     format: if development { LogFormat::Text } else { LogFormat::Json },
/// };
/// let config = TelemetryConfig::from_lookup_with("APP", |_| None, defaults)?;
/// assert_eq!((config.filter.as_str(), config.format), ("info,sqlx=warn", LogFormat::Text));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    /// `EnvFilter` directive when neither `<PREFIX>_LOG` nor `RUST_LOG` is set.
    pub filter: String,
    /// Format when `<PREFIX>_LOG_FORMAT` is unset.
    pub format: LogFormat,
}

impl Default for Defaults {
    /// [`DEFAULT_FILTER`] and JSON.
    fn default() -> Self {
        Self {
            filter: DEFAULT_FILTER.to_owned(),
            format: LogFormat::Json,
        }
    }
}

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
                          empty: `RUST_LOG`, then `info` (or the app's default). An invalid directive is a \
                          configuration error.",
        },
        ConfigVar {
            suffix: "LOG_FORMAT",
            default: Some("json"),
            description: "`json` (one JSON object per line, for log collectors) or `text` (readable text, \
                          one line per event, for local work). Unset or empty: `json` (or the app's \
                          default).",
        },
    ];

    /// Reads `<prefix>_LOG` (unset or empty: `RUST_LOG`, then [`DEFAULT_FILTER`]) and
    /// `<prefix>_LOG_FORMAT` (unset or empty: `json`).
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        Self::from_lookup_with(prefix, lookup, Defaults::default())
    }

    /// Like [`TelemetryConfig::from_lookup`] with the app's `defaults`: `<prefix>_LOG` (unset or
    /// empty: `RUST_LOG`, then `defaults.filter`) and `<prefix>_LOG_FORMAT` (unset or empty:
    /// `defaults.format`). An invalid default filter is an error on `<prefix>_LOG`.
    pub fn from_lookup_with(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
        defaults: Defaults,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, &lookup);
        // `RUST_LOG` read through a lookup with prefix `RUST`, so its errors name `RUST_LOG`.
        let rust = Lookup::new("RUST", &lookup);
        let (source, filter, reason) = match (l.get("LOG"), rust.get("LOG")) {
            (Some(f), _) => (&l, f, "not a valid EnvFilter directive"),
            (None, Some(f)) => (&rust, f, "not a valid EnvFilter directive"),
            (None, None) => (
                &l,
                defaults.filter,
                "unset, and the app's default is not a valid EnvFilter directive",
            ),
        };
        build_filter(&filter).map_err(|_| source.invalid("LOG", reason))?;
        let format = match l
            .get("LOG_FORMAT")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None => defaults.format,
            Some("json") => LogFormat::Json,
            Some("text") => LogFormat::Text,
            Some(_) => return Err(l.invalid("LOG_FORMAT", "expected json or text")),
        };
        Ok(Self { format, filter })
    }

    /// Installs the global subscriber; see [`init`].
    pub fn init(&self) -> Result<InitOutcome, InvalidFilter> {
        init(self.format, &self.filter)
    }
}

/// What [`init`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InitOutcome {
    /// This call installed the subscriber; records of the `log` crate are forwarded to it.
    Installed,
    /// This call installed the subscriber, but another `log` logger was already set, so records of the
    /// `log` crate keep going there.
    InstalledWithoutLog,
    /// A global subscriber was already set (an earlier call, a test harness); it stays, nothing changed.
    AlreadyInstalled,
}

/// Installs the global subscriber with `format` and `filter` and forwards records of the `log` crate to
/// it. Idempotent: a second call returns [`InitOutcome::AlreadyInstalled`].
pub fn init(format: LogFormat, filter: &str) -> Result<InitOutcome, InvalidFilter> {
    let builder = tracing_subscriber::fmt().with_env_filter(build_filter(filter)?);
    let installed = match format {
        LogFormat::Json => set_global(builder.json().flatten_event(true).finish()),
        LogFormat::Text => set_global(builder.finish()),
    };
    if !installed {
        return Ok(InitOutcome::AlreadyInstalled);
    }
    // After the subscriber, so the `log` bridge gets its level (as tracing-subscriber's `try_init`).
    let level = tracing_log::AsLog::as_log(&tracing::level_filters::LevelFilter::current());
    let bridged = tracing_log::LogTracer::builder()
        .with_max_level(level)
        .init()
        .is_ok();
    Ok(if bridged {
        InitOutcome::Installed
    } else {
        InitOutcome::InstalledWithoutLog
    })
}

/// Sets `subscriber` as the global default; `false` if one is already set.
fn set_global(subscriber: impl tracing::Subscriber + Send + Sync + 'static) -> bool {
    tracing::subscriber::set_global_default(subscriber).is_ok()
}

/// The `EnvFilter` of `directive`.
fn build_filter(directive: &str) -> Result<EnvFilter, InvalidFilter> {
    EnvFilter::try_new(directive).map_err(|_| InvalidFilter)
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
