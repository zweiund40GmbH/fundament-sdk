//! The `returnTo` of the login: where the browser goes after a successful login. Only a path on this
//! origin is accepted, anything else becomes `/` – an open redirect would let a phishing link end on a
//! foreign site after a genuine login.
//!
//! Accepted is a value of at most [`MAX_LEN`] visible ASCII characters (`!`–`~`) that starts with exactly
//! one `/` and contains no `\`. That refuses absolute URLs (`https://…`), protocol-relative ones
//! (`//host`), the backslash forms browsers treat like `/` (`/\host`, `\\host`), control characters
//! (tab and newline are dropped by URL parsers: `/\t/host` would become `//host`), spaces and
//! non-ASCII characters. Query and fragment are kept.

/// Longest accepted return path.
pub const MAX_LEN: usize = 512;

/// The safe return path for `raw` (`/` when absent or refused).
pub fn sanitize(raw: Option<&str>) -> String {
    match raw {
        Some(p) if is_safe(p) => p.to_owned(),
        _ => "/".to_owned(),
    }
}

/// Whether `p` is an acceptable return path (see the module docs).
pub fn is_safe(p: &str) -> bool {
    let b = p.as_bytes();
    p.len() <= MAX_LEN
        && b.first() == Some(&b'/')
        && b.get(1) != Some(&b'/')
        && b.iter().all(|c| c.is_ascii_graphic() && *c != b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_kept() {
        for good in [
            "/",
            "/orders",
            "/orders/7?tab=items&x=%2F%2Fhost#top",
            "/a/b/",
            "/%2F%2Fhost",
            "/@host",
            "/a//b",
            "/?next=https://example.com",
        ] {
            assert_eq!(sanitize(Some(good)), good, "{good:?}");
        }
        assert_eq!(sanitize(None), "/");
    }

    #[test]
    fn hostile_values_become_root() {
        let long = format!("/{}", "a".repeat(MAX_LEN));
        for bad in [
            "",
            "orders",
            "./orders",
            "../orders",
            "https://evil.example",
            "http:/evil.example",
            "javascript:alert(1)",
            "data:text/html,x",
            "//evil.example",
            "///evil.example",
            "/\\evil.example",
            "\\\\evil.example",
            "\\/evil.example",
            "/a\\b",
            "/\t/evil.example",
            "/\n/evil.example",
            "/\r\nSet-Cookie: x=1",
            "/a\u{0}b",
            "/a\u{7f}b",
            "/a\u{85}b",
            "/a b",
            "/ä",
            "/\u{2028}",
            " /orders",
            "\u{feff}/orders",
            long.as_str(),
        ] {
            assert_eq!(sanitize(Some(bad)), "/", "{bad:?}");
        }
        assert!(is_safe(&format!("/{}", "a".repeat(MAX_LEN - 1))));
    }
}
