//! Bearer keys on the wire: reading the key of an incoming `Authorization` header and holding the key
//! this app sends to another app.

use crate::SecretString;
use crate::config::{ConfigError, ConfigErrorKind, Lookup};
use std::fmt;

/// The key of an `Authorization` header value `Bearer <key>` (scheme case-insensitive, surrounding
/// spaces ignored), or `None` for any other form.
pub fn bearer_token(header_value: &str) -> Option<&str> {
    let (scheme, key) = header_value.split_once(' ')?;
    let key = key.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !key.is_empty()).then_some(key)
}

/// A key is not usable in a header (empty, or not visible ASCII).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the key must be 1 or more visible ASCII characters")]
pub struct InvalidKey;

/// The key this app sends as `Authorization: Bearer <key>`. `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct BearerKey(SecretString);

impl fmt::Debug for BearerKey {
    /// Never prints the key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerKey(***)")
    }
}

impl BearerKey {
    /// Wraps `key` after checking it fits into a header: only visible ASCII, no spaces.
    pub fn new(key: impl Into<String>) -> Result<Self, InvalidKey> {
        let key = key.into();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(InvalidKey);
        }
        Ok(Self(SecretString::new(key)))
    }

    /// Reads the key from `<prefix>_<suffix>`; `Ok(None)` when unset.
    pub fn from_lookup(
        prefix: &str,
        suffix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        l.get(suffix)
            .map(|k| {
                Self::new(k)
                    .map_err(|e| l.error(suffix, ConfigErrorKind::InvalidKey(e), e.to_string()))
            })
            .transpose()
    }

    /// The key. Only for the request header.
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_forms() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer  abc "), Some("abc"));
        for bad in ["Basic abc", "Bearer", "Bearer  ", "abc", ""] {
            assert_eq!(bearer_token(bad), None, "{bad}");
        }
    }

    #[test]
    fn outgoing_key() {
        let k = BearerKey::new("s3cr3t-key").unwrap();
        assert_eq!(k.expose(), "s3cr3t-key");
        assert!(!format!("{k:?}").contains("s3cr3t"));
        for bad in ["", "with space", "line\nbreak", "umläut"] {
            assert_eq!(BearerKey::new(bad), Err(InvalidKey), "{bad:?}");
        }
        let env = |n: &str| (n == "APP_KEY").then(|| " s3cr3t ".to_owned());
        assert_eq!(
            BearerKey::from_lookup("APP", "KEY", env)
                .unwrap()
                .map(|k| k.expose().to_owned()),
            Some("s3cr3t".to_owned())
        );
        assert_eq!(BearerKey::from_lookup("APP", "OTHER", env), Ok(None));
        let bad = |_: &str| Some("two words".to_owned());
        let err = BearerKey::from_lookup("APP", "KEY", bad).unwrap_err();
        assert!(!err.to_string().contains("words"));
    }
}
