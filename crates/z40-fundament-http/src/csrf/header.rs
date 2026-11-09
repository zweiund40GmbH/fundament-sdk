//! [`CsrfHeader`]: the header name and value of the guard, checked at compile time when built in a
//! const context, so [`super::Csrf::header_static`] cannot fail at run time.

use super::CORS_SAFELISTED;

/// A CSRF header name and value checked by a `const fn`: declare it as a `const` (or in a
/// `const { … }` block) and a bad name or value is a compile error, not a start-up error.
///
/// The name must be lowercase (a header token of `a-z 0-9 ! # $ % & ' * + - . ^ _ ` | ~`) and not
/// CORS-safelisted ([`CORS_SAFELISTED`]); the value non-empty visible ASCII.
///
/// ```
/// use z40_fundament_http::csrf::{Csrf, CsrfHeader};
///
/// const CSRF: CsrfHeader = CsrfHeader::new("x-requested-with", "crm");
/// let layer = Csrf::header_static(CSRF).protect_prefix("/api/");
/// ```
///
/// A safelisted name does not compile:
///
/// ```compile_fail
/// use z40_fundament_http::csrf::{Csrf, CsrfHeader};
///
/// const CSRF: CsrfHeader = CsrfHeader::new("content-type", "crm");
/// let layer = Csrf::header_static(CSRF);
/// ```
///
/// Neither does an empty value:
///
/// ```compile_fail
/// use z40_fundament_http::csrf::{Csrf, CsrfHeader};
///
/// let layer = Csrf::header_static(const { CsrfHeader::new("x-requested-with", "") });
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsrfHeader {
    pub(super) name: &'static str,
    pub(super) value: &'static str,
}

impl CsrfHeader {
    /// Checks `name` and `value` (rules in the type docs).
    ///
    /// # Panics
    ///
    /// On a bad name or value. In a const context that is a compile error; called at run time it
    /// panics, so declare it as a `const`.
    pub const fn new(name: &'static str, value: &'static str) -> Self {
        assert!(
            valid_name(name),
            "CSRF header name must be a lowercase header token and not CORS-safelisted"
        );
        assert!(
            valid_value(value),
            "CSRF header value must be non-empty visible ASCII"
        );
        Self { name, value }
    }

    /// The header name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The header value.
    pub const fn value(&self) -> &'static str {
        self.value
    }
}

/// Whether `c` may appear in a lowercase header name (RFC 9110 `tchar` without uppercase).
const fn name_byte(c: u8) -> bool {
    c.is_ascii_lowercase()
        || c.is_ascii_digit()
        || matches!(
            c,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Byte equality usable in a `const fn`.
const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// A non-empty lowercase header token that is not CORS-safelisted.
pub(super) const fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if !name_byte(b[i]) {
            return false;
        }
        i += 1;
    }
    let mut j = 0;
    while j < CORS_SAFELISTED.len() {
        if same(b, CORS_SAFELISTED[j].as_bytes()) {
            return false;
        }
        j += 1;
    }
    true
}

/// Non-empty visible ASCII.
pub(super) const fn valid_value(value: &str) -> bool {
    let b = value.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_graphic() {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderName;

    #[test]
    fn names_and_values() {
        for ok in ["x-requested-with", "x-csrf", "a", "x!#$%&'*+-.^_`|~09"] {
            assert!(valid_name(ok), "{ok}");
            // Every accepted name is one `HeaderName::from_static` takes (it panics otherwise).
            assert_eq!(HeaderName::from_static(ok).as_str(), ok);
        }
        for bad in ["", "X-Csrf", "x csrf", "x:csrf", "x\u{e4}", "x/y", "x\"y"] {
            assert!(!valid_name(bad), "{bad}");
        }
        for safelisted in CORS_SAFELISTED {
            assert!(!valid_name(safelisted), "{safelisted}");
        }
        assert!(valid_value("crm") && valid_value("a!~"));
        for bad in ["", "a b", "a\tb", "\u{e4}", "a\n"] {
            assert!(!valid_value(bad), "{bad:?}");
        }
    }

    #[test]
    #[should_panic(expected = "CORS-safelisted")]
    fn run_time_call_with_a_bad_name_panics() {
        let name = String::from("accept").leak();
        let _ = CsrfHeader::new(name, "crm");
    }
}
