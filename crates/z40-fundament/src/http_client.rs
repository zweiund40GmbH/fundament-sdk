//! The outgoing HTTP side shared by the feed consumer and the status client: origin validation, one
//! client builder (TLS with the built-in Mozilla roots **and** the platform store, which honours
//! `SSL_CERT_FILE`; no redirects, so a key never travels to another location), a bounded body reader
//! and the cause chain of transport errors for logs. Requests carry keys only in a header, so neither
//! URLs nor error messages ever contain them.

use std::time::Duration;

/// The origin of `raw` (`scheme://host[:port]`, no trailing slash) if it is exactly an origin of
/// `scheme`: no user info, no path (one trailing `/` allowed), no query, no fragment.
fn origin(raw: &str, scheme: &str) -> Option<reqwest::Url> {
    let raw = raw.trim();
    // The URL parser silently repairs forms like `https:///host`; the raw text must already be
    // `scheme://authority` with at most one trailing slash.
    let rest = raw.strip_prefix(scheme)?.strip_prefix("://")?;
    if rest.trim_end_matches('/').contains('/') || rest.ends_with("//") {
        return None;
    }
    reqwest::Url::parse(raw).ok().filter(|u| {
        u.scheme() == scheme
            && u.host_str().is_some_and(|h| !h.is_empty())
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/"
            && u.query().is_none()
            && u.fragment().is_none()
    })
}

/// The `https://` origin of `raw`, serialized without trailing slash.
pub(crate) fn https_origin(raw: &str) -> Option<String> {
    origin(raw, "https").map(|u| u.origin().ascii_serialization())
}

/// The `http://` origin of `raw` if its host is a loopback address (tests only).
pub(crate) fn loopback_http_origin(raw: &str) -> Option<String> {
    let u = origin(raw, "http")?;
    let host = u.host_str()?;
    let ip = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost"
        || ip
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback());
    loopback.then(|| u.origin().ascii_serialization())
}

/// The built-in Mozilla roots (`webpki-root-certs`) as reqwest certificates.
fn built_in_roots() -> impl Iterator<Item = reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        // The bundled roots are valid DER; a root reqwest could not take is left out, not fatal.
        .filter_map(|der| reqwest::Certificate::from_der(der).ok())
}

/// An HTTP client with the given timeouts that follows no redirects; `https_only` refuses plain http.
/// TLS trusts the platform store and the built-in Mozilla roots.
pub(crate) fn build(
    connect_timeout: Duration,
    total_timeout: Duration,
    https_only: bool,
) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .tls_certs_merge(built_in_roots())
        .connect_timeout(connect_timeout)
        .timeout(total_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(https_only)
        .user_agent(concat!("z40-fundament/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// `e` with all its sources joined by `: ` (TLS, DNS, connect …), for logs.
pub(crate) fn cause_chain(e: &reqwest::Error) -> String {
    let mut cause = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        cause.push_str(": ");
        cause.push_str(&s.to_string());
        source = s.source();
    }
    cause
}

/// Why a body could not be read.
#[derive(Debug)]
pub(crate) enum BodyError {
    /// More than the allowed bytes (announced or sent).
    TooLarge,
    /// The connection failed or timed out while reading.
    Transport(reqwest::Error),
}

/// Reads at most `max` bytes of `res`; a larger announced length is refused before reading.
pub(crate) async fn read_body(
    mut res: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, BodyError> {
    if res.content_length().is_some_and(|n| n > max as u64) {
        return Err(BodyError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(BodyError::Transport)? {
        if body.len() + chunk.len() > max {
            return Err(BodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert_eq!(
            https_origin("https://crm.example.com/").as_deref(),
            Some("https://crm.example.com")
        );
        for bad in [
            "http://crm.example.com",
            "https://crm.example.com/feed",
            "https://user:pw@crm.example.com",
            "https:///crm.example.com",
            "https://crm.example.com?x=1",
            "https://crm.example.com#x",
        ] {
            assert!(https_origin(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn loopback_only_for_plain_http() {
        assert_eq!(
            loopback_http_origin("http://127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert!(loopback_http_origin("http://localhost:1").is_some());
        assert!(loopback_http_origin("http://[::1]:1").is_some());
        assert!(loopback_http_origin("http://crm.example.com").is_none());
        assert!(loopback_http_origin("https://127.0.0.1").is_none());
        assert!(https_origin("http://127.0.0.1").is_none());
    }
}
