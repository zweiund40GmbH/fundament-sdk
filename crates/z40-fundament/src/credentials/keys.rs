//! The list of accepted keys: comma-separated `name=sha256hex` entries. A presented key is hashed
//! with SHA-256 and compared with **every** entry in constant time, so neither the position of a match
//! nor a partial match shows in the timing.

use crate::config::{ConfigError, Lookup};
use sha2::{Digest, Sha256};
use std::fmt;
use subtle::ConstantTimeEq;

/// Longest client name.
const NAME_MAX: usize = 64;

/// One client with the hash of its key.
#[derive(Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    hash: [u8; 32],
}

/// The configured clients; empty = every request is refused.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct KeyList(Vec<Entry>);

impl fmt::Debug for KeyList {
    /// Names only (hashes are not secret, but have no use in logs).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|e| &e.name))
            .finish()
    }
}

/// Why a key list was rejected. Names the entry only by its position (1-based): an entry's content is
/// never echoed, because an operator may have pasted the bare key instead of `name=hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyListError {
    /// Not of the form `name=<64 hex characters>`.
    #[error("entry {0} is invalid (expected name=sha256hex)")]
    Invalid(usize),
    /// The name is not 1–64 characters of `a-z A-Z 0-9 _ . -`.
    #[error("entry {0} has an invalid name (allowed: 1-64 characters a-z, A-Z, 0-9, _ . -)")]
    InvalidName(usize),
    /// The name occurs twice.
    #[error("entry {0} repeats a name")]
    Duplicate(usize),
}

/// Whether `name` is a valid client name: 1–64 characters of `a-z A-Z 0-9 _ . -`.
pub fn name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// SHA-256 of a key.
fn hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

/// Lower-case hex of `bytes`.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses exactly 64 hex characters (any case).
fn parse_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The list entry for a key: `name=sha256hex`. `None` if the name is invalid. Operators configure the
/// entry at the receiving app and give the key only to the client.
pub fn key_entry(name: &str, key: &str) -> Option<String> {
    name_is_valid(name).then(|| format!("{name}={}", to_hex(&hash(key))))
}

impl KeyList {
    /// Parses the comma-separated `name=sha256hex` list (blank entries are ignored).
    pub fn parse(raw: &str) -> Result<Self, KeyListError> {
        let mut out: Vec<Entry> = Vec::new();
        let entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
        for (i, entry) in entries.enumerate() {
            let nr = i + 1;
            let (name, hex) = entry.split_once('=').ok_or(KeyListError::Invalid(nr))?;
            let name = name.trim();
            if !name_is_valid(name) {
                return Err(KeyListError::InvalidName(nr));
            }
            let hash = parse_hex(hex.trim()).ok_or(KeyListError::Invalid(nr))?;
            if out.iter().any(|e| e.name == name) {
                return Err(KeyListError::Duplicate(nr));
            }
            out.push(Entry {
                name: name.to_owned(),
                hash,
            });
        }
        Ok(Self(out))
    }

    /// Reads the list from the variable `<prefix>_<suffix>` (unset = empty list).
    pub fn from_lookup(
        prefix: &str,
        suffix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        Self::parse(&l.get(suffix).unwrap_or_default())
            .map_err(|e| l.invalid(suffix, e.to_string()))
    }

    /// Whether no client is configured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The configured names in their order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|e| e.name.as_str())
    }

    /// The name of the client whose key is `key`, or `None`. Compares the hash with every entry in
    /// constant time and does not stop at a match.
    pub fn verify(&self, key: &str) -> Option<&str> {
        let h = hash(key);
        let mut found = None;
        for e in &self.0 {
            if bool::from(e.hash.ct_eq(&h)) {
                found = Some(e.name.as_str());
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_their_entry() {
        let a = key_entry("crm", "k1").unwrap();
        let b = key_entry("Billing.2", "k2").unwrap();
        assert_eq!(a.len(), "crm=".len() + 64);
        let list = KeyList::parse(&format!("{a}, {b}")).unwrap();
        assert_eq!(list.verify("k1"), Some("crm"));
        assert_eq!(list.verify("k2"), Some("Billing.2"));
        assert_eq!(list.verify("k3"), None);
        assert_eq!(list.verify(""), None);
        assert_eq!(list.verify("K1"), None, "keys are case-sensitive");
        assert_eq!(format!("{list:?}"), r#"["crm", "Billing.2"]"#);
        assert_eq!(list.names().collect::<Vec<_>>(), ["crm", "Billing.2"]);
        let upper = format!("a={}", to_hex(&hash("k")).to_ascii_uppercase());
        assert_eq!(KeyList::parse(&upper).unwrap().verify("k"), Some("a"));
        assert!(key_entry("with space", "k").is_none() && key_entry("", "k").is_none());
    }

    #[test]
    fn empty_lists() {
        assert!(KeyList::parse("").unwrap().is_empty());
        let blank = KeyList::parse(" , ").unwrap();
        assert!(blank.is_empty() && blank.verify("k").is_none());
    }

    #[test]
    fn invalid_lists() {
        let h = "a".repeat(64);
        assert_eq!(
            KeyList::parse(&format!("a={h},a={h}")),
            Err(KeyListError::Duplicate(2))
        );
        for bad in ["a", "a=", "a=xyz", "=abc", "ä=x"] {
            assert!(KeyList::parse(bad).is_err(), "{bad}");
        }
        assert!(KeyList::parse(&format!("a={}", "g".repeat(64))).is_err());
        assert!(KeyList::parse(&format!("a={}", "a".repeat(63))).is_err());
        assert_eq!(
            KeyList::parse(&format!("{}={h}", "n".repeat(65))),
            Err(KeyListError::InvalidName(1))
        );
        assert_eq!(
            KeyList::parse(&format!("a={h},b")),
            Err(KeyListError::Invalid(2))
        );
    }

    #[test]
    fn bare_key_is_never_echoed() {
        let key = "very-secret-key-value";
        for raw in [key.to_owned(), format!("a={key}"), format!("{key}={key}")] {
            let err = KeyList::parse(&raw).unwrap_err().to_string();
            assert!(!err.contains(key), "key echoed: {err}");
        }
        let env = |_: &str| Some(format!("x={key}"));
        let err = KeyList::from_lookup("APP", "STATUS_CLIENTS", env).unwrap_err();
        assert_eq!(err.name, "APP_STATUS_CLIENTS");
        assert!(!err.to_string().contains(key));
    }
}
