//! The rules a Content-Security-Policy of this crate must follow (security baseline): scripts are
//! restricted (`script-src` or `default-src` is present), never with `'unsafe-inline'`, and
//! `frame-ancestors` is named.

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
    /// A script directive allows `'unsafe-inline'`.
    #[error("the CSP allows 'unsafe-inline' for scripts")]
    UnsafeInlineScripts,
    /// No `frame-ancestors` directive (framing would be allowed everywhere).
    #[error("the CSP has no frame-ancestors directive")]
    NoFrameAncestors,
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

/// [`check_csp`] with the derived values.
pub(crate) fn check(csp: &str) -> Result<CheckedCsp, CspError> {
    let value = HeaderValue::from_str(csp.trim()).map_err(|_| CspError::InvalidValue)?;
    let directives: Vec<(String, Vec<&str>)> = csp
        .split(';')
        .filter_map(|d| {
            let mut tokens = d.split_whitespace();
            let name = tokens.next()?.to_ascii_lowercase();
            Some((name, tokens.collect()))
        })
        .collect();
    let sources = |name: &str| directives.iter().find(|(n, _)| n == name).map(|(_, s)| s);
    let script_fallback = sources("script-src").or_else(|| sources("default-src"));
    let Some(fallback) = script_fallback else {
        return Err(CspError::UnrestrictedScripts);
    };
    let unsafe_inline =
        |s: &&Vec<&str>| s.iter().any(|t| t.eq_ignore_ascii_case("'unsafe-inline'"));
    let script_lists = [
        Some(fallback),
        sources("script-src-elem"),
        sources("script-src-attr"),
    ];
    if script_lists.iter().flatten().any(unsafe_inline) {
        return Err(CspError::UnsafeInlineScripts);
    }
    let Some(ancestors) = sources("frame-ancestors") else {
        return Err(CspError::NoFrameAncestors);
    };
    let frame_options = match ancestors.as_slice() {
        [t] if t.eq_ignore_ascii_case("'none'") => Some(HeaderValue::from_static("DENY")),
        [t] if t.eq_ignore_ascii_case("'self'") => Some(HeaderValue::from_static("SAMEORIGIN")),
        _ => None,
    };
    Ok(CheckedCsp {
        value,
        frame_options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_policies() {
        let c = check(super::super::API_CSP).unwrap();
        assert_eq!(c.frame_options.unwrap(), "DENY");
        let web = "default-src 'self'; script-src 'self' 'sha256-abc='; style-src 'self' 'unsafe-inline'; \
                   frame-ancestors 'self'";
        assert_eq!(check(web).unwrap().frame_options.unwrap(), "SAMEORIGIN");
        assert_eq!(
            check("default-src 'self'; frame-ancestors https://example.com")
                .unwrap()
                .frame_options,
            None
        );
    }

    #[test]
    fn refused_policies() {
        let e = |csp: &str| check(csp).unwrap_err();
        assert_eq!(
            e("img-src 'self'; frame-ancestors 'none'"),
            CspError::UnrestrictedScripts
        );
        assert_eq!(
            e("default-src 'self' 'unsafe-inline'; frame-ancestors 'none'"),
            CspError::UnsafeInlineScripts
        );
        assert_eq!(
            e(
                "default-src 'self' 'unsafe-inline'; script-src 'self'; script-src-attr 'UNSAFE-INLINE'; frame-ancestors 'none'"
            ),
            CspError::UnsafeInlineScripts
        );
        assert_eq!(e("default-src 'self'"), CspError::NoFrameAncestors);
        assert_eq!(
            e("default-src 'self';\n frame-ancestors 'none'"),
            CspError::InvalidValue
        );
        assert!(
            check("default-src 'self' 'unsafe-inline'; script-src 'self'; frame-ancestors 'none'")
                .is_ok()
        );
    }
}
