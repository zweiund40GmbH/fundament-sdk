//! The limit on login starts: each `GET <base>/login` writes a row to the store before anyone is
//! authenticated, so starts are counted per client address and in total, in fixed windows of
//! [`WINDOW`]. A refused start writes nothing.
//!
//! The counters live in the process (like the failure brake of `z40-fundament`): they bound the rows a
//! single instance writes; with several instances the effective limit is that many times higher.
//! Residual risk: an attacker flooding from many addresses (many /64s) can use up the total and block
//! new logins for everyone for the rest of the window; existing sessions are unaffected. With the
//! default resolver behind a reverse proxy every user shares the proxy's counter (warned once).
//!
//! Memory is bounded to [`MAX_TRACKED`] addresses: when full, expired windows are dropped first; if it
//! is still full, new addresses are only held by the total limit. IPv6 addresses count by their /64
//! (`brake_address`).

use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use z40_fundament::credentials::brake_address;

/// Length of a window.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Most tracked addresses.
const MAX_TRACKED: usize = 10_000;

/// Resolves the client address of a request, e.g. `TrustedProxies::resolver()`.
pub type ClientIpFn = Arc<dyn Fn(&Parts) -> Option<IpAddr> + Send + Sync>;

/// The default resolver: the TCP peer from `ConnectInfo<SocketAddr>`.
pub(crate) fn connect_info() -> ClientIpFn {
    Arc::new(|parts: &Parts| {
        parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
    })
}

/// Starts in one window.
#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

impl Window {
    /// The count at `now` (0 once the window has passed).
    fn current(&self, now: Instant) -> u32 {
        if now.duration_since(self.start) < WINDOW {
            self.count
        } else {
            0
        }
    }

    /// Counts one start at `now`, opening a new window if the old one has passed.
    fn hit(&mut self, now: Instant) {
        if now.duration_since(self.start) >= WINDOW {
            *self = Window {
                start: now,
                count: 0,
            };
        }
        self.count = self.count.saturating_add(1);
    }
}

/// Counters of [`LoginLimit`].
#[derive(Debug)]
struct Counters {
    total: Window,
    per_ip: HashMap<IpAddr, Window>,
}

/// Login starts per client address and in total, per [`WINDOW`].
pub(crate) struct LoginLimit {
    per_ip: u32,
    total: u32,
    counters: Mutex<Counters>,
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
    /// default resolver, the TCP peer).
    pub(crate) fn new(per_ip: u32, total: u32, client_ip: Option<ClientIpFn>) -> Self {
        let default_resolver = client_ip.is_none();
        Self {
            per_ip,
            total,
            counters: Mutex::new(Counters {
                total: Window {
                    start: Instant::now(),
                    count: 0,
                },
                per_ip: HashMap::new(),
            }),
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

    /// Whether a start from the request `parts` is allowed; an allowed start is counted.
    pub(crate) fn allow(&self, parts: &Parts) -> bool {
        self.allow_at(self.address(parts), Instant::now())
    }

    /// [`LoginLimit::allow`] for address `ip` at `now`.
    pub(crate) fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let ip = brake_address(ip);
        let mut c = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        let used = c.per_ip.get(&ip).map_or(0, |w| w.current(now));
        if c.total.current(now) >= self.total || used >= self.per_ip {
            return false;
        }
        c.total.hit(now);
        if c.per_ip.len() >= MAX_TRACKED && !c.per_ip.contains_key(&ip) {
            c.per_ip.retain(|_, w| now.duration_since(w.start) < WINDOW);
            if c.per_ip.len() >= MAX_TRACKED {
                return true;
            }
        }
        c.per_ip
            .entry(ip)
            .or_insert(Window {
                start: now,
                count: 0,
            })
            .hit(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn per_address_and_total_in_windows() {
        let l = LoginLimit::new(2, 3, None);
        let t = Instant::now();
        assert!(l.allow_at(ip("192.0.2.1"), t) && l.allow_at(ip("192.0.2.1"), t));
        assert!(!l.allow_at(ip("192.0.2.1"), t), "per address");
        assert!(l.allow_at(ip("192.0.2.2"), t));
        assert!(!l.allow_at(ip("192.0.2.3"), t), "total");
        let later = t + WINDOW;
        assert!(l.allow_at(ip("192.0.2.1"), later), "new window");
    }

    #[test]
    fn ipv6_counts_by_its_64_and_refusals_are_not_counted() {
        let l = LoginLimit::new(1, 100, None);
        let t = Instant::now();
        assert!(l.allow_at(ip("2001:db8::1"), t));
        assert!(!l.allow_at(ip("2001:db8::2"), t), "same /64");
        assert!(l.allow_at(ip("2001:db8:0:1::1"), t));
        for _ in 0..50 {
            assert!(!l.allow_at(ip("2001:db8::1"), t));
        }
        let c = l.counters.lock().unwrap();
        assert_eq!(c.total.count, 2, "refused starts do not use up the total");
    }

    #[test]
    fn memory_is_bounded() {
        let l = LoginLimit::new(5, u32::MAX, None);
        let t = Instant::now();
        for i in 0..(MAX_TRACKED as u32 + 100) {
            assert!(l.allow_at(IpAddr::V4(Ipv4Addr::from_bits(i)), t));
        }
        assert_eq!(l.counters.lock().unwrap().per_ip.len(), MAX_TRACKED);
        // Expired windows make room again.
        assert!(l.allow_at(ip("203.0.113.9"), t + WINDOW));
        assert_eq!(l.counters.lock().unwrap().per_ip.len(), 1);
    }

    #[test]
    fn unknown_address_shares_one_bucket() {
        let l = LoginLimit::new(1, 100, Some(Arc::new(|_: &Parts| None)));
        let (parts, ()) = axum::http::Request::new(()).into_parts();
        assert!(l.allow(&parts));
        assert!(!l.allow(&parts));
    }

    /// Request parts whose TCP peer is `peer`.
    fn from_peer(peer: &str) -> Parts {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        let addr: SocketAddr = format!("{peer}:443").parse().unwrap();
        parts.extensions.insert(ConnectInfo(addr));
        parts
    }

    #[test]
    fn private_peer_with_the_default_resolver_is_warned_once() {
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
        let l = LoginLimit::new(100, 100, None);
        assert!(l.allow(&from_peer("203.0.113.5")));
        assert!(!l.warned_proxy_peer.load(Ordering::Relaxed), "public peer");
        assert!(l.allow(&from_peer("10.0.0.1")));
        assert!(l.warned_proxy_peer.load(Ordering::Relaxed));
        let explicit = LoginLimit::new(100, 100, Some(connect_info()));
        assert!(explicit.allow(&from_peer("10.0.0.1")));
        assert!(
            !explicit.warned_proxy_peer.load(Ordering::Relaxed),
            "app chose it"
        );
    }
}
