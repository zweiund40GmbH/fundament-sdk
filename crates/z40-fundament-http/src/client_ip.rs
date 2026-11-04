//! The client address of a request behind reverse proxies, for rate limits, failure brakes and logs.
//!
//! `X-Forwarded-For` is only believed when the direct peer (the TCP connection, axum's
//! `ConnectInfo<SocketAddr>`) is a configured proxy (`<PREFIX>_TRUSTED_PROXIES`); anyone else could send
//! the header and pick their own address. The header is then walked **from the right** (each proxy
//! appends the address it saw) while the hops are trusted proxies; the first untrusted hop is the
//! client. An unparsable hop stops the walk at the last trusted address. Several header lines count as
//! one comma-separated list. The standard `Forwarded` header is not read.
//!
//! Serve the app with `into_make_service_with_connect_info::<SocketAddr>()`, otherwise the peer is
//! unknown.
//!
//! The endpoints of `z40-fundament` take the resolver as their `client_ip` function, so their failure
//! brake sees the same address as the app's own limits:
//!
//! ```
//! use z40_fundament::credentials::KeyList;
//! use z40_fundament::status::{StatusEndpoint, StatusProvider};
//! use z40_fundament_http::client_ip::TrustedProxies;
//!
//! fn status_router<P: StatusProvider>(provider: P, keys: KeyList, proxies: &TrustedProxies) -> axum::Router {
//!     StatusEndpoint::new(provider, keys).client_ip(proxies.resolver()).router()
//! }
//! ```

mod net;

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use net::Net;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use z40_fundament::config::{ConfigError, ConfigVar, Lookup};

/// Name of the forwarding header.
pub const X_FORWARDED_FOR: &str = "x-forwarded-for";

/// The configured proxies (addresses and CIDR networks). Empty = no proxy is trusted, the peer is the
/// client. Cheap to clone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Arc<[Net]>);

/// An entry of a proxy list that is neither an address nor a CIDR network. The entry itself is not
/// part of the message (configuration errors never echo values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "entry {position} is not an IP address or a CIDR network with a prefix length of at least 1"
)]
pub struct InvalidProxyList {
    /// 1-based position of the first invalid entry.
    pub position: usize,
}

impl TrustedProxies {
    /// The variables of [`TrustedProxies::from_lookup`].
    pub const VARIABLES: &'static [ConfigVar] = &[ConfigVar {
        suffix: "TRUSTED_PROXIES",
        default: None,
        description: "Reverse proxies whose `X-Forwarded-For` is believed, as comma-separated addresses \
                      or CIDR networks (`10.0.0.0/8, fd00::/8`). Unset or empty: no proxy is trusted and \
                      the TCP peer is the client. List only your own proxies: every listed address can \
                      choose the client address, so a broad range makes `X-Forwarded-For` spoofable \
                      (prefix length 0 is refused).",
    }];

    /// Reads `<prefix>_TRUSTED_PROXIES`.
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        let suffix = Self::VARIABLES[0].suffix;
        Self::parse(&l.get(suffix).unwrap_or_default())
            .map_err(|e| l.invalid(suffix, e.to_string()))
    }

    /// Parses a comma-separated list of addresses and CIDR networks; empty entries are skipped.
    pub fn parse(raw: &str) -> Result<Self, InvalidProxyList> {
        let entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
        let nets = entries
            .enumerate()
            .map(|(i, e)| Net::parse(e).ok_or(InvalidProxyList { position: i + 1 }))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(nets.into()))
    }

    /// Whether no proxy is configured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `ip` is a trusted proxy.
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|n| n.contains(ip))
    }

    /// The client address for a connection from `peer` with the `X-Forwarded-For` value `forwarded_for`
    /// (see the module docs for the walk).
    pub fn client(&self, peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
        let peer = net::canonical(peer);
        if !self.contains(peer) {
            return peer;
        }
        let mut client = peer;
        for hop in forwarded_for.unwrap_or_default().rsplit(',').map(str::trim) {
            let Ok(ip) = hop.parse::<IpAddr>() else {
                break;
            };
            client = net::canonical(ip);
            if !self.contains(client) {
                break;
            }
        }
        client
    }

    /// The client address of a request: the peer from `ConnectInfo<SocketAddr>`, replaced via
    /// `X-Forwarded-For` only behind a trusted proxy. `None` without `ConnectInfo`.
    pub fn client_ip(&self, parts: &Parts) -> Option<IpAddr> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>()?.0.ip();
        Some(self.client(peer, forwarded_for(&parts.headers).as_deref()))
    }

    /// [`TrustedProxies::client_ip`] as an owned function, e.g. for `StatusEndpoint::client_ip` and
    /// `FeedEndpoint::client_ip` of `z40-fundament`.
    pub fn resolver(&self) -> impl Fn(&Parts) -> Option<IpAddr> + Send + Sync + 'static {
        let proxies = self.clone();
        move |parts: &Parts| proxies.client_ip(parts)
    }
}

/// All `X-Forwarded-For` lines joined with `,` (lines that are not visible ASCII are skipped).
fn forwarded_for(headers: &HeaderMap) -> Option<String> {
    let lines: Vec<&str> = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    (!lines.is_empty()).then(|| lines.join(","))
}

/// Extractor of the client address. Needs [`TrustedProxies`] in the router state (`FromRef`). Without
/// `ConnectInfo` (e.g. in tests) the address is `0.0.0.0`, so all such requests share one bucket; the
/// first such request logs a warning (like the endpoints of `z40-fundament`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    TrustedProxies: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    /// Never rejects.
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Infallible> {
        let proxies = TrustedProxies::from_ref(state);
        Ok(Self(
            proxies.client_ip(parts).unwrap_or_else(unknown_address),
        ))
    }
}

/// Whether [`unknown_address`] has warned (once per process).
static WARNED_UNKNOWN: AtomicBool = AtomicBool::new(false);

/// `0.0.0.0`, with a warning on the first call.
fn unknown_address() -> IpAddr {
    if !WARNED_UNKNOWN.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "ClientIp cannot see client addresses: all clients share one address; \
             serve with into_make_service_with_connect_info"
        );
    }
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parse_and_lookup() {
        let t = TrustedProxies::parse("10.0.0.0/8, ,192.168.1.1, fd00::/8").unwrap();
        assert!(
            t.contains(ip("10.1.2.3")) && t.contains(ip("fd12::1")) && !t.contains(ip("11.0.0.1"))
        );
        assert!(TrustedProxies::parse(" ").unwrap().is_empty());
        assert_eq!(
            TrustedProxies::parse("10.0.0.0/8, secret-host").unwrap_err(),
            InvalidProxyList { position: 2 }
        );
        assert_eq!(
            TrustedProxies::parse("10.0.0.0/8, fd00::/8, ::/0").unwrap_err(),
            InvalidProxyList { position: 3 },
            "trusting everyone is refused"
        );
        let all = |k: &str| (k == "APP_TRUSTED_PROXIES").then(|| "0.0.0.0/0".to_owned());
        assert!(TrustedProxies::from_lookup("APP", all).is_err());
        let env = |k: &str| (k == "APP_TRUSTED_PROXIES").then(|| "proxy.example.com".to_owned());
        let err = TrustedProxies::from_lookup("APP", env).unwrap_err();
        assert_eq!(err.name, "APP_TRUSTED_PROXIES");
        assert!(
            !err.to_string().contains("proxy.example.com"),
            "value never echoed"
        );
        assert!(
            TrustedProxies::from_lookup("APP", |_| None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn walk_from_the_right() {
        let t = TrustedProxies::parse("10.0.0.0/8").unwrap();
        assert_eq!(
            t.client(ip("203.0.113.9"), Some("1.2.3.4")),
            ip("203.0.113.9"),
            "untrusted peer"
        );
        assert_eq!(
            t.client(ip("10.0.0.2"), Some("6.6.6.6, 1.2.3.4, 10.0.0.5")),
            ip("1.2.3.4")
        );
        assert_eq!(t.client(ip("10.0.0.2"), None), ip("10.0.0.2"));
        assert_eq!(t.client(ip("10.0.0.2"), Some("garbage")), ip("10.0.0.2"));
        assert_eq!(
            t.client(ip("10.0.0.2"), Some("1.2.3.4, garbage, 10.0.0.5")),
            ip("10.0.0.5")
        );
        assert_eq!(
            t.client(ip("10.0.0.2"), Some("10.0.0.7")),
            ip("10.0.0.7"),
            "all hops trusted"
        );
        assert_eq!(
            t.client(ip("::ffff:10.0.0.2"), Some("1.2.3.4")),
            ip("1.2.3.4"),
            "mapped peer"
        );
        assert_eq!(
            t.client(ip("::ffff:203.0.113.9"), Some("1.2.3.4")),
            ip("203.0.113.9")
        );
    }
}
