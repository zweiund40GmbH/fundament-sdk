//! The rules a Content-Security-Policy of this crate must follow (security baseline):
//!
//! * scripts are restricted: `script-src` or `default-src` is present;
//! * the script directives (`script-src`, `script-src-elem`, `script-src-attr`, and `default-src` when it
//!   stands in for `script-src`) contain no unsafe source: `'unsafe-inline'`, `'unsafe-eval'`,
//!   `'unsafe-hashes'`, the wildcard `*`, `data:` or another bare scheme such as `https:` (which allows
//!   every host). Nonces, hashes, `'self'`, `'strict-dynamic'`, `'none'` and explicit hosts are fine;
//! * `frame-ancestors` is named and is not open to every site (`*` or a bare scheme).

use axum::http::HeaderValue;

/// Why a CSP is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CspError {
    /// Not a valid header value (control characters, non-ASCII).
    #[error("the CSP is not a valid header value")]
    InvalidValue,
    /// Neither `script-src` nor `default-src`: scripts would be unrestricted.
    #[error("the CSP has neither script-src nor default-src")]
    UnrestrictedScripts,
    /// A script directive allows an unsafe source: `'unsafe-inline'`, `'unsafe-eval'`, `'unsafe-hashes'`,
    /// `*`, `data:` or a bare scheme (`https:`).
    #[error("a script directive of the CSP allows an unsafe source")]
    UnsafeScriptSource,
    /// No `frame-ancestors` directive (framing would be allowed everywhere).
    #[error("the CSP has no frame-ancestors directive")]
    NoFrameAncestors,
    /// `frame-ancestors` allows every site (`*` or a bare scheme).
    #[error("frame-ancestors of the CSP allows every site")]
    OpenFrameAncestors,
}

/// A CSP that passed [`check_csp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckedCsp {
    /// The header value.
    pub(crate) value: HeaderValue,
    /// The matching `X-Frame-Options`, if `frame-ancestors` has one.
    pub(crate) frame_options: Option<HeaderValue>,
}

/// Checks `csp` against the rules of the module. Public so apps can test the CSPs their routes set
/// themselves.
pub fn check_csp(csp: &str) -> Result<(), CspError> {
    check(csp).map(|_| ())
}

/// The directives of a CSP: lower-case name and source tokens (a repeated directive is ignored, as
/// browsers do).
fn directives(csp: &str) -> Vec<(String, Vec<&str>)> {
    let mut out: Vec<(String, Vec<&str>)> = Vec::new();
    for d in csp.split(';') {
        let mut tokens = d.split_whitespace();
        let Some(name) = tokens.next().map(str::to_ascii_lowercase) else {
            continue;
        };
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name, tokens.collect()));
        }
    }
    out
}

/// The sources of directive `name`, if present.
fn sources<'a>(directives: &'a [(String, Vec<&'a str>)], name: &str) -> Option<&'a [&'a str]> {
    directives
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, s)| s.as_slice())
}

/// Whether `token` is a bare scheme like `https:` or `data:` (allows every resource of that scheme).
fn is_bare_scheme(token: &str) -> bool {
    token.strip_suffix(':').is_some_and(|scheme| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}

/// Whether a script source token is unsafe (see the module docs).
fn is_unsafe_script_source(token: &str) -> bool {
    const KEYWORDS: [&str; 3] = ["'unsafe-inline'", "'unsafe-eval'", "'unsafe-hashes'"];
    KEYWORDS.iter().any(|k| token.eq_ignore_ascii_case(k)) || token == "*" || is_bare_scheme(token)
}

/// The `X-Frame-Options` equivalent of `frame-ancestors` sources: `'none'` → `DENY`, `'self'` →
/// `SAMEORIGIN`, anything else has none.
fn frame_options_of(ancestors: &[&str]) -> Option<HeaderValue> {
    match ancestors {
        [t] if t.eq_ignore_ascii_case("'none'") => Some(HeaderValue::from_static("DENY")),
        [t] if t.eq_ignore_ascii_case("'self'") => Some(HeaderValue::from_static("SAMEORIGIN")),
        _ => None,
    }
}

/// The `X-Frame-Options` matching the `frame-ancestors` of any CSP (e.g. one a route set itself).
pub(crate) fn frame_options(csp: &str) -> Option<HeaderValue> {
    frame_options_of(sources(&directives(csp), "frame-ancestors")?)
}

/// [`check_csp`] with the derived values.
pub(crate) fn check(csp: &str) -> Result<CheckedCsp, CspError> {
    let value = HeaderValue::from_str(csp.trim()).map_err(|_| CspError::InvalidValue)?;
    let list = directives(csp);
    let get = |name: &str| sources(&list, name);
    let fallback = get("script-src")
        .or_else(|| get("default-src"))
        .ok_or(CspError::UnrestrictedScripts)?;
    let script_lists = [
        Some(fallback),
        get("script-src-elem"),
        get("script-src-attr"),
    ];
    let tokens = script_lists.into_iter().flatten().flatten();
    if tokens.into_iter().any(|t| is_unsafe_script_source(t)) {
        return Err(CspError::UnsafeScriptSource);
    }
    let ancestors = get("frame-ancestors").ok_or(CspError::NoFrameAncestors)?;
    if ancestors.iter().any(|t| *t == "*" || is_bare_scheme(t)) {
        return Err(CspError::OpenFrameAncestors);
    }
    Ok(CheckedCsp {
        value,
        frame_options: frame_options_of(ancestors),
    })
}

#[cfg(test)]
#[path = "csp_tests.rs"]
mod tests;
