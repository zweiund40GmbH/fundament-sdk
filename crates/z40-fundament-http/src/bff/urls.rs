//! The URL rules of [`super::config`]: `https://`, or `http://` to a loopback host when the app allows
//! it for local development; never user info or a fragment.

use url::{Host, Url};

/// A URL of the configuration breaks the rules of [`super::config`]; names the field, never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid URL in {0} (https://; http:// only for loopback hosts with allow_loopback_http; no user \
     info or fragment)"
)]
pub struct InvalidUrl(pub &'static str);

/// The error kind of a bad URL.
pub(crate) fn url_kind() -> z40_fundament::config::ConfigErrorKind {
    z40_fundament::config::ConfigErrorKind::InvalidUrl
}

/// `https://` URL, or `http://` to a loopback host when `allow_loopback_http`; no user info, no fragment.
pub(crate) fn check_url(raw: &str, allow_loopback_http: bool) -> Result<(), &'static str> {
    let url = Url::parse(raw).map_err(|_| "not a URL")?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("no user info or fragment allowed");
    }
    let loopback = match url.host() {
        Some(Host::Domain(d)) => d == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if loopback && allow_loopback_http => Ok(()),
        "http" if loopback => Err("http:// to a loopback host only with allow_loopback_http \
                                  (development)"),
        _ => Err("expected https:// (http:// only for loopback hosts in development)"),
    }
}
