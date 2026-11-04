//! Secrets that must never end up in logs: a string type whose `Debug` output is redacted and which
//! has no `Display`, so it cannot be formatted into a message by accident.

use std::fmt;

/// A secret string (key, token, password). `Debug` prints `SecretString(***)`; the value is only
/// reachable through [`SecretString::expose`], which makes every use visible in review.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps `value`.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value. Only pass it to the place that needs it (e.g. a request header).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    /// Never prints the value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let s = SecretString::new("hunter2");
        assert_eq!(format!("{s:?}"), "SecretString(***)");
        assert_eq!(s.expose(), "hunter2");
    }
}
