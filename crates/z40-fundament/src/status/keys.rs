//! `<PREFIX>_STATUS_KEYS`: one status key per target app as comma-separated `app=key` entries. A key is
//! only ever sent to its own app, so an app (or a test instance) that is polled cannot collect the keys
//! of the others.

use crate::config::{ConfigError, ConfigErrorKind, ConfigVar, Lookup};
use crate::credentials::{BearerKey, name_is_valid};
use std::fmt;

/// Why a status key map was rejected. Names the entry only by its position (1-based), never its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StatusKeysError {
    /// Not of the form `app=key`.
    #[error("entry {0} is invalid (expected app=key)")]
    Invalid(usize),
    /// The app name is not 1–64 characters of `a-z A-Z 0-9 _ . -`.
    #[error("entry {0} has an invalid app name (allowed: 1-64 characters a-z, A-Z, 0-9, _ . -)")]
    InvalidName(usize),
    /// The key is empty or not visible ASCII.
    #[error("entry {0} has an invalid key (1 or more visible ASCII characters)")]
    InvalidKey(usize),
    /// The app occurs twice.
    #[error("entry {0} repeats an app")]
    Duplicate(usize),
}

/// The status key of every target app; empty = nothing to poll. `Debug` shows the app names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct StatusKeys(Vec<(String, BearerKey)>);

impl fmt::Debug for StatusKeys {
    /// App names only; keys are never printed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StatusKeys")
            .field(&self.apps().collect::<Vec<_>>())
            .finish()
    }
}

impl StatusKeys {
    /// The variables read by [`StatusKeys::from_lookup`] (documented in `docs/configuration.md`).
    pub const VARIABLES: &'static [ConfigVar] = &[ConfigVar {
        suffix: "STATUS_KEYS",
        default: None,
        description: "Status keys of the apps this app polls, as comma-separated `app=key` entries (one \
            key per app; a key is only sent to its own app). Unset or empty: no app is polled. Secret.",
    }];

    /// Parses the comma-separated `app=key` list (blank entries are ignored; the key is everything after
    /// the first `=`).
    pub fn parse(raw: &str) -> Result<Self, StatusKeysError> {
        let mut out: Vec<(String, BearerKey)> = Vec::new();
        let entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
        for (i, entry) in entries.enumerate() {
            let nr = i + 1;
            let (app, key) = entry.split_once('=').ok_or(StatusKeysError::Invalid(nr))?;
            let app = app.trim();
            if !name_is_valid(app) {
                return Err(StatusKeysError::InvalidName(nr));
            }
            let key = BearerKey::new(key.trim()).map_err(|_| StatusKeysError::InvalidKey(nr))?;
            if out.iter().any(|(a, _)| a == app) {
                return Err(StatusKeysError::Duplicate(nr));
            }
            out.push((app.to_owned(), key));
        }
        Ok(Self(out))
    }

    /// Reads `<prefix>_STATUS_KEYS` (unset = empty map).
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        let suffix = "STATUS_KEYS";
        Self::parse(&l.get(suffix).unwrap_or_default())
            .map_err(|e| l.error(suffix, ConfigErrorKind::StatusKeys(e), e.to_string()))
    }

    /// The key of `app`, if it has one.
    pub fn get(&self, app: &str) -> Option<&BearerKey> {
        self.0.iter().find(|(a, _)| a == app).map(|(_, k)| k)
    }

    /// The apps with a key, in their configured order.
    pub fn apps(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(a, _)| a.as_str())
    }

    /// Whether no app has a key.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_per_app() {
        let k = StatusKeys::parse(" crm = key-of-crm-1 , billing.2=a=b, ,").unwrap();
        assert_eq!(k.get("crm").map(BearerKey::expose), Some("key-of-crm-1"));
        assert_eq!(k.get("billing.2").map(BearerKey::expose), Some("a=b"));
        assert!(k.get("other").is_none() && k.get("CRM").is_none());
        assert_eq!(k.apps().collect::<Vec<_>>(), ["crm", "billing.2"]);
        assert_eq!(format!("{k:?}"), r#"StatusKeys(["crm", "billing.2"])"#);
        assert!(StatusKeys::parse("").unwrap().is_empty());
        assert!(StatusKeys::default().is_empty());
    }

    #[test]
    fn invalid_maps() {
        for (raw, err) in [
            ("bare-key", StatusKeysError::Invalid(1)),
            ("crm=k,a b=k", StatusKeysError::InvalidName(2)),
            ("=k", StatusKeysError::InvalidName(1)),
            ("crm=", StatusKeysError::InvalidKey(1)),
            ("crm=two words", StatusKeysError::InvalidKey(1)),
            ("crm=k1,crm=k2", StatusKeysError::Duplicate(2)),
        ] {
            assert_eq!(StatusKeys::parse(raw), Err(err), "{raw}");
        }
        let long = format!("{}=k", "n".repeat(65));
        assert_eq!(
            StatusKeys::parse(&long),
            Err(StatusKeysError::InvalidName(1))
        );
    }

    #[test]
    fn config_never_echoes_a_key() {
        let on = StatusKeys::from_lookup("MON", |n| {
            (n == "MON_STATUS_KEYS").then(|| "crm=s3cr3t".to_owned())
        })
        .unwrap();
        assert_eq!(on.get("crm").map(BearerKey::expose), Some("s3cr3t"));
        assert!(StatusKeys::from_lookup("MON", |_| None).unwrap().is_empty());
        let secret = "very-secret-key-value";
        for raw in [secret.to_owned(), format!("crm={secret} x")] {
            let err = StatusKeys::from_lookup("MON", |_| Some(raw.clone())).unwrap_err();
            assert_eq!(err.name, "MON_STATUS_KEYS");
            assert!(matches!(err.kind, ConfigErrorKind::StatusKeys(_)));
            assert!(std::error::Error::source(&err).is_some());
            assert!(!err.to_string().contains(secret), "key echoed: {err}");
        }
    }
}
