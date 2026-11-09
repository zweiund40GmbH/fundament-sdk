//! The limit on login starts: each `GET <base>/login` writes a row to the store before anyone is
//! authenticated, so starts are counted per client address and in total, in fixed windows of
//! [`WINDOW`]. A refused start writes nothing.
//!
//! By default the counters live in the process (`local`, like the failure brake of `z40-fundament`):
//! they bound the rows a single instance writes; with several instances the effective limit is that
//! many times higher. A [`LoginLimitStore`] (`shared`, e.g. `bff-mysql`) shares them between the
//! instances. Residual risk: an attacker flooding from many addresses (many /64s) can use up the total
//! and block new logins for everyone for the rest of the window; existing sessions are unaffected. With
//! the default resolver behind a reverse proxy every user shares the proxy's counter (warned once).
//!
//! IPv6 addresses count by their /64 (`brake_address`).

mod local;
mod shared;

pub(crate) use shared::DynLimitStore;
pub use shared::LoginLimitStore;

use axum::http::request::Parts;
use local::LocalCounters;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use z40_fundament::StoreError;
use z40_fundament::credentials::brake_address;

/// Length of a window.
pub const WINDOW: Duration = Duration::from_secs(60);

/// Resolves the client address of a request, e.g. `TrustedProxies::resolver()`.
pub type ClientIpFn = Arc<dyn Fn(&Parts) -> Option<IpAddr> + Send + Sync>;

/// The default resolver: the TCP peer from `ConnectInfo<SocketAddr>` (or `MockConnectInfo` in tests).
pub(crate) fn connect_info() -> ClientIpFn {
    Arc::new(crate::client_ip::peer)
}

/// Where the counters live.
enum Counters {
    /// In this process (the default).
    Local(LocalCounters),
    /// In a store shared by all instances.
    Shared(Box<dyn DynLimitStore>),
}

/// Login starts per client address and in total, per [`WINDOW`].
pub(crate) struct LoginLimit {
    per_ip: u32,
    total: u32,
    counters: Counters,
    client_ip: ClientIpFn,
    /// Whether `client_ip` is the default (the TCP peer), not one the app passed.
    default_resolver: bool,
    warned_unknown_ip: AtomicBool,
    warned_proxy_peer: AtomicBool,
}

/// Whether `ip` is a loopback, private or link-local address: as the TCP peer that is usually a reverse
/// proxy, so with the default resolver all users would share its counter.
pub(crate) fn looks_like_proxy(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

impl LoginLimit {
    /// A limit of `per_ip` and `total` starts per window, addresses from `client_ip` (`None`: the
    /// default resolver, the TCP peer), counters in `store` (`None`: in this process).
    pub(crate) fn new(
        per_ip: u32,
        total: u32,
        client_ip: Option<ClientIpFn>,
        store: Option<Box<dyn DynLimitStore>>,
    ) -> Self {
        let default_resolver = client_ip.is_none();
        Self {
            per_ip,
            total,
            counters: match store {
                Some(store) => Counters::Shared(store),
                None => Counters::Local(LocalCounters::new()),
            },
            client_ip: client_ip.unwrap_or_else(connect_info),
            default_resolver,
            warned_unknown_ip: AtomicBool::new(false),
            warned_proxy_peer: AtomicBool::new(false),
        }
    }

    /// The address of `parts`, or `0.0.0.0` (one shared bucket, warned once) when unknown.
    fn address(&self, parts: &Parts) -> IpAddr {
        if let Some(ip) = (self.client_ip)(parts) {
            if self.default_resolver
                && looks_like_proxy(ip)
                && !self.warned_proxy_peer.swap(true, Ordering::Relaxed)
            {
                tracing::warn!(
                    "login limit sees a loopback or private peer address, probably a reverse proxy: \
                     all users behind it share one counter; pass client_ip(TrustedProxies::resolver())"
                );
            }
            return ip;
        }
        if !self.warned_unknown_ip.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "login cannot see client addresses: all clients share one login limit; serve with \
                 into_make_service_with_connect_info or pass client_ip"
            );
        }
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    }

    /// Whether a start from the request `parts` is allowed; an allowed start is counted. Fails only
    /// when a shared store fails.
    pub(crate) async fn allow(&self, parts: &Parts) -> Result<bool, StoreError> {
        let ip = brake_address(self.address(parts));
        match &self.counters {
            Counters::Local(c) => Ok(c.allow_at(ip, self.per_ip, self.total, Instant::now())),
            Counters::Shared(store) => {
                let now = chrono::Utc::now();
                shared::allow_at(store.as_ref(), ip, self.per_ip, self.total, now).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::extract::connect_info::MockConnectInfo;
    use std::net::SocketAddr;

    #[tokio::test]
    async fn ipv6_counts_by_its_64() {
        let l = LoginLimit::new(1, 100, None, None);
        assert!(l.allow(&from_peer("2001:db8::1")).await.unwrap());
        assert!(
            !l.allow(&from_peer("2001:db8::2")).await.unwrap(),
            "same /64"
        );
        assert!(l.allow(&from_peer("2001:db8:0:1::1")).await.unwrap());
    }

    #[tokio::test]
    async fn unknown_address_shares_one_bucket() {
        let l = LoginLimit::new(1, 100, Some(Arc::new(|_: &Parts| None)), None);
        let (parts, ()) = axum::http::Request::new(()).into_parts();
        assert!(l.allow(&parts).await.unwrap());
        assert!(!l.allow(&parts).await.unwrap());
    }

    /// Request parts whose TCP peer is `peer`.
    fn from_peer(peer: &str) -> Parts {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        let addr = SocketAddr::new(peer.parse().unwrap(), 443);
        parts.extensions.insert(ConnectInfo(addr));
        parts
    }

    #[tokio::test]
    async fn private_peer_with_the_default_resolver_is_warned_once() {
        for (ip, proxy) in [
            ("127.0.0.1", true),
            ("10.1.2.3", true),
            ("192.168.0.1", true),
            ("169.254.1.1", true),
            ("::1", true),
            ("fd00::1", true),
            ("fe80::1", true),
            ("::ffff:10.0.0.1", true),
            ("203.0.113.5", false),
            ("2001:db8::1", false),
        ] {
            assert_eq!(looks_like_proxy(ip.parse().unwrap()), proxy, "{ip}");
        }
        let l = LoginLimit::new(100, 100, None, None);
        assert!(l.allow(&from_peer("203.0.113.5")).await.unwrap());
        assert!(!l.warned_proxy_peer.load(Ordering::Relaxed), "public peer");
        assert!(l.allow(&from_peer("10.0.0.1")).await.unwrap());
        assert!(l.warned_proxy_peer.load(Ordering::Relaxed));
        let mut mocked = axum::http::Request::new(()).into_parts().0;
        let peer: SocketAddr = "10.0.0.2:443".parse().unwrap();
        mocked.extensions.insert(MockConnectInfo(peer));
        assert_eq!(connect_info()(&mocked), Some(peer.ip()), "router tests");
        let explicit = LoginLimit::new(100, 100, Some(connect_info()), None);
        assert!(explicit.allow(&from_peer("10.0.0.1")).await.unwrap());
        assert!(
            !explicit.warned_proxy_peer.load(Ordering::Relaxed),
            "app chose it"
        );
    }
}
