//! Configuration from environment-like lookups. Every setting of this crate is read through a
//! `from_lookup(prefix, lookup)` function that takes `Fn(&str) -> Option<String>` instead of the
//! process environment, so it is testable without touching `std::env`. The app chooses the prefix
//! (e.g. `CRM_FEED` → `CRM_FEED_URL`).
//!
//! Each feature lists its variables as [`ConfigVar`]s; [`reference_markdown`] renders them into the
//! table of `docs/configuration.md` (a test keeps that file in sync – one source of truth).

use std::ops::RangeInclusive;

/// An invalid setting. The message names the variable but never echoes its value, because an
/// operator may have pasted a secret into the wrong variable. [`ConfigError::kind`] tells apps what
/// went wrong, e.g. to word their own message; `reason` is the English text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConfigError {
    /// Full variable name, e.g. `CRM_FEED_INTERVAL_SECS`.
    pub name: String,
    /// What is wrong, without the value (English).
    pub reason: String,
    /// What is wrong, as data.
    pub kind: ConfigErrorKind,
}

/// The kind of a [`ConfigError`]. More kinds may come in minor versions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigErrorKind {
    /// The value is not valid for the variable (e.g. from [`Lookup::invalid`] of an app).
    InvalidValue,
    /// Not an integer in `min..=max`.
    OutOfRange {
        /// Smallest allowed value.
        min: u64,
        /// Largest allowed value.
        max: u64,
    },
    /// Not an origin of the expected form (e.g. `https://host[:port]`).
    InvalidUrl,
    /// A key list (`name=sha256hex,…`) is invalid.
    #[cfg(feature = "credentials")]
    KeyList(crate::credentials::KeyListError),
    /// A key cannot be sent in a header.
    #[cfg(feature = "credentials")]
    InvalidKey(crate::credentials::InvalidKey),
    /// A status key map (`app=key,…`) is invalid.
    #[cfg(feature = "status-client")]
    StatusKeys(crate::status::StatusKeysError),
}

impl std::fmt::Display for ConfigError {
    /// `invalid setting <NAME>: <reason>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid setting {}: {}", self.name, self.reason)
    }
}

impl std::error::Error for ConfigError {
    /// The key list, key or key map error behind it, if any.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            #[cfg(feature = "credentials")]
            ConfigErrorKind::KeyList(e) => Some(e),
            #[cfg(feature = "credentials")]
            ConfigErrorKind::InvalidKey(e) => Some(e),
            #[cfg(feature = "status-client")]
            ConfigErrorKind::StatusKeys(e) => Some(e),
            _ => None,
        }
    }
}

/// One documented variable of a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigVar {
    /// Name after the prefix and `_`, e.g. `URL`.
    pub suffix: &'static str,
    /// Default value; `None` = unset by default.
    pub default: Option<&'static str>,
    /// What the variable does.
    pub description: &'static str,
}

/// Reads prefixed variables from a lookup function (`<prefix>_<suffix>`). Values are trimmed; empty
/// counts as unset. Public so apps can read their own settings the same way.
pub struct Lookup<'a, F> {
    prefix: &'a str,
    lookup: F,
}

impl<'a, F: Fn(&str) -> Option<String>> Lookup<'a, F> {
    /// Lookup of `<prefix>_<suffix>` variables.
    pub fn new(prefix: &'a str, lookup: F) -> Self {
        Self { prefix, lookup }
    }

    /// Full name of `suffix`.
    pub fn name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix)
    }

    /// Trimmed value of `suffix`, `None` if unset or empty.
    pub fn get(&self, suffix: &str) -> Option<String> {
        (self.lookup)(&self.name(suffix))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    /// An error for `suffix` of kind [`ConfigErrorKind::InvalidValue`].
    pub fn invalid(&self, suffix: &str, reason: impl Into<String>) -> ConfigError {
        self.error(suffix, ConfigErrorKind::InvalidValue, reason)
    }

    /// An error for `suffix` of `kind`.
    pub fn error(
        &self,
        suffix: &str,
        kind: ConfigErrorKind,
        reason: impl Into<String>,
    ) -> ConfigError {
        ConfigError {
            name: self.name(suffix),
            reason: reason.into(),
            kind,
        }
    }

    /// Integer `suffix` within `range`, `default` when unset.
    pub fn number(
        &self,
        suffix: &str,
        default: u64,
        range: RangeInclusive<u64>,
    ) -> Result<u64, ConfigError> {
        let Some(raw) = self.get(suffix) else {
            return Ok(default);
        };
        let (min, max) = (*range.start(), *range.end());
        let err = || {
            let reason = format!("expected an integer in {min}..={max}");
            self.error(suffix, ConfigErrorKind::OutOfRange { min, max }, reason)
        };
        let n: u64 = raw.parse().map_err(|_| err())?;
        if range.contains(&n) {
            Ok(n)
        } else {
            Err(err())
        }
    }
}

/// The markdown table of `vars` for `prefix` (placeholder `<PREFIX>` in the docs).
pub fn variable_table(prefix: &str, vars: &[ConfigVar]) -> String {
    let mut out = String::from("| Variable | Default | Description |\n|---|---|---|\n");
    for v in vars {
        let default = v.default.map_or("(unset)".to_owned(), |d| format!("`{d}`"));
        out.push_str(&format!(
            "| `{prefix}_{}` | {default} | {} |\n",
            v.suffix, v.description
        ));
    }
    out
}

/// The generated part of `docs/configuration.md`: one table per enabled feature that has settings.
pub fn reference_markdown() -> String {
    #[allow(unused_mut)]
    let mut out = String::new();
    #[cfg(feature = "feed-consumer")]
    {
        out.push_str("### Feed consumer (`feed-consumer`)\n\n");
        out.push_str("Read with `FeedConsumerConfig::from_lookup(prefix, lookup)`.\n\n");
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::consumer::FeedConsumerConfig::VARIABLES,
        ));
        out.push('\n');
    }
    #[cfg(feature = "feed-source")]
    {
        out.push_str("### Feed source (`feed-source`)\n\n");
        out.push_str("Read with `FeedSourceConfig::from_lookup(prefix, lookup)`.\n\n");
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::feed_source::FeedSourceConfig::VARIABLES,
        ));
        out.push('\n');
    }
    #[cfg(feature = "status")]
    {
        out.push_str("### Status endpoint (`status`)\n\n");
        out.push_str("Read with `StatusConfig::from_lookup(prefix, lookup)`.\n\n");
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::status::StatusConfig::VARIABLES,
        ));
        out.push('\n');
    }
    #[cfg(feature = "status-client")]
    {
        out.push_str("### Status client (`status-client`)\n\n");
        out.push_str("Read with `StatusKeys::from_lookup(prefix, lookup)`.\n\n");
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::status::StatusKeys::VARIABLES,
        ));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn numbers_with_default_and_range() {
        let l = Lookup::new(
            "APP",
            env(&[("APP_N", " 45 "), ("APP_BAD", "s3cr3t"), ("APP_E", "")]),
        );
        assert_eq!(l.number("N", 60, 30..=3600), Ok(45));
        assert_eq!(l.number("MISSING", 60, 30..=3600), Ok(60));
        assert_eq!(l.number("E", 60, 30..=3600), Ok(60), "empty = unset");
        let err = l.number("BAD", 60, 30..=3600).unwrap_err();
        assert_eq!(err.name, "APP_BAD");
        assert!(!err.to_string().contains("s3cr3t"), "value never echoed");
        assert_eq!(
            l.number("N", 60, 50..=60).unwrap_err().kind,
            ConfigErrorKind::OutOfRange { min: 50, max: 60 }
        );
        assert_eq!(err.kind, ConfigErrorKind::OutOfRange { min: 30, max: 3600 });
        assert_eq!(l.invalid("X", "bad").kind, ConfigErrorKind::InvalidValue);
        assert_eq!(
            l.invalid("X", "bad").to_string(),
            "invalid setting APP_X: bad"
        );
    }

    #[test]
    fn table_lists_variables() {
        let vars = [ConfigVar {
            suffix: "URL",
            default: None,
            description: "Origin.",
        }];
        let t = variable_table("P", &vars);
        assert!(t.contains("| `P_URL` | (unset) | Origin. |"));
    }
}
