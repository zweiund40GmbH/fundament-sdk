//! Configuration from environment-like lookups. Every setting of this crate is read through a
//! `from_lookup(prefix, lookup)` function that takes `Fn(&str) -> Option<String>` instead of the
//! process environment, so it is testable without touching `std::env`. The app chooses the prefix
//! (e.g. `CRM_FEED` → `CRM_FEED_URL`).
//!
//! Each feature lists its variables as [`ConfigVar`]s; [`reference_markdown`] renders them into the
//! table of `docs/configuration.md` (a test keeps that file in sync – one source of truth).

use std::ops::RangeInclusive;

/// An invalid setting. The message names the variable but never echoes its value, because an
/// operator may have pasted a secret into the wrong variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid setting {name}: {reason}")]
pub struct ConfigError {
    /// Full variable name, e.g. `CRM_FEED_INTERVAL_SECS`.
    pub name: String,
    /// What is wrong, without the value.
    pub reason: String,
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

    /// An error for `suffix`.
    pub fn invalid(&self, suffix: &str, reason: impl Into<String>) -> ConfigError {
        ConfigError {
            name: self.name(suffix),
            reason: reason.into(),
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
        let reason = || format!("expected an integer in {}..={}", range.start(), range.end());
        let n: u64 = raw.parse().map_err(|_| self.invalid(suffix, reason()))?;
        if range.contains(&n) {
            Ok(n)
        } else {
            Err(self.invalid(suffix, reason()))
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
        assert!(l.number("N", 60, 50..=60).is_err());
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
