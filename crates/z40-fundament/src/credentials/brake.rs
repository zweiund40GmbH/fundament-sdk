//! The brake on failed attempts: more than [`MAX_FAILURES`] failed attempts per client address within
//! [`WINDOW`] are refused (`429`). Per process and in memory: it only slows down guessing – keys are long
//! random values and carry the security – so it needs no shared state.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Failed attempts per address and window before requests are refused.
pub const MAX_FAILURES: u32 = 10;
/// Length of the window.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Most tracked addresses (bounds the memory): when full, expired windows are dropped first; if it is
/// still full, new addresses are not tracked.
const MAX_TRACKED: usize = 10_000;

/// The address the brake counts for `ip`: IPv4 as is, IPv6 by its /64 (one host usually holds a whole
/// /64, so single addresses would not brake it), IPv4-mapped IPv6 as IPv4.
pub fn brake_address(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !(u128::MAX >> 64))),
        v4 => v4,
    }
}

/// `ip` shortened for logs (client addresses are personal data): IPv4 to its /24, IPv6 to its /48,
/// e.g. `203.0.113.0/24`.
pub fn shortened_for_logs(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(v4) => format!("{}/24", Ipv4Addr::from_bits(v4.to_bits() & 0xffff_ff00)),
        IpAddr::V6(v6) => format!(
            "{}/48",
            Ipv6Addr::from_bits(v6.to_bits() & !(u128::MAX >> 48))
        ),
    }
}

/// One window of failed attempts.
#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    failures: u32,
}

/// Failed attempts per client address in fixed windows. Share one instance (e.g. in an `Arc`) between
/// all endpoints that check the same kind of key.
#[derive(Debug, Default)]
pub struct FailureBrake(Mutex<HashMap<IpAddr, Window>>);

impl FailureBrake {
    /// A brake without recorded failures.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `ip` already failed [`MAX_FAILURES`] times in its current window.
    pub fn is_blocked(&self, ip: IpAddr, now: Instant) -> bool {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&brake_address(ip))
            .is_some_and(|w| now.duration_since(w.start) < WINDOW && w.failures >= MAX_FAILURES)
    }

    /// Counts a failed attempt of `ip`.
    pub fn record_failure(&self, ip: IpAddr, now: Instant) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let ip = brake_address(ip);
        if map.len() >= MAX_TRACKED && !map.contains_key(&ip) {
            map.retain(|_, w| now.duration_since(w.start) < WINDOW);
            if map.len() >= MAX_TRACKED {
                return;
            }
        }
        let w = map.entry(ip).or_insert(Window {
            start: now,
            failures: 0,
        });
        if now.duration_since(w.start) >= WINDOW {
            *w = Window {
                start: now,
                failures: 0,
            };
        }
        w.failures = w.failures.saturating_add(1);
    }

    /// Number of tracked addresses (for tests and metrics).
    pub fn tracked(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn brake_per_address_and_window() {
        let b = FailureBrake::new();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILURES {
            assert!(!b.is_blocked(ip("203.0.113.9"), t0));
            b.record_failure(ip("203.0.113.9"), t0);
        }
        assert!(
            b.is_blocked(ip("203.0.113.9"), t0),
            "the 11th attempt is refused"
        );
        assert!(!b.is_blocked(ip("203.0.113.10"), t0));
        let later = t0 + WINDOW;
        assert!(!b.is_blocked(ip("203.0.113.9"), later), "new window");
        b.record_failure(ip("203.0.113.9"), later);
        assert!(!b.is_blocked(ip("203.0.113.9"), later));
    }

    #[test]
    fn ipv6_counts_per_64() {
        let b = FailureBrake::new();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILURES {
            b.record_failure(ip("2001:db8:1:2::1"), t0);
        }
        assert!(b.is_blocked(ip("2001:db8:1:2:ffff::9"), t0));
        assert!(!b.is_blocked(ip("2001:db8:1:3::1"), t0));
        assert_eq!(brake_address(ip("::ffff:203.0.113.9")), ip("203.0.113.9"));
    }

    #[test]
    fn memory_is_bounded() {
        let b = FailureBrake::new();
        let t0 = Instant::now();
        for i in 0..MAX_TRACKED as u32 {
            b.record_failure(IpAddr::V4(Ipv4Addr::from_bits(i)), t0);
        }
        let new = IpAddr::V4(Ipv4Addr::from_bits(u32::MAX));
        for _ in 0..MAX_FAILURES {
            b.record_failure(new, t0);
        }
        assert!(!b.is_blocked(new, t0), "full map: new address not tracked");
        assert_eq!(b.tracked(), MAX_TRACKED);
        let known = IpAddr::V4(Ipv4Addr::from_bits(0));
        for _ in 1..MAX_FAILURES {
            b.record_failure(known, t0);
        }
        assert!(b.is_blocked(known, t0), "known addresses are still counted");
        b.record_failure(new, t0 + WINDOW);
        assert_eq!(b.tracked(), 1, "expired windows dropped");
    }

    #[test]
    fn shortened() {
        assert_eq!(shortened_for_logs(ip("203.0.113.9")), "203.0.113.0/24");
        assert_eq!(
            shortened_for_logs(ip("::ffff:203.0.113.9")),
            "203.0.113.0/24"
        );
        assert_eq!(
            shortened_for_logs(ip("2001:db8:1:2:3:4:5:6")),
            "2001:db8:1::/48"
        );
    }
}
