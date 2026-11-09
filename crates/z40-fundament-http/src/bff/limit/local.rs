//! The in-process counters of the login limit (the default): fixed windows of [`WINDOW`] per client
//! address and in total, memory bounded to [`MAX_TRACKED`] addresses. When full, expired windows are
//! dropped first; if it is still full, new addresses are only held by the total limit.

use super::WINDOW;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// Most tracked addresses.
pub(super) const MAX_TRACKED: usize = 10_000;

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

/// The counters behind [`LocalCounters`].
#[derive(Debug)]
struct Counters {
    total: Window,
    per_ip: HashMap<IpAddr, Window>,
}

/// Login starts per client address (already reduced to its /64 for IPv6) and in total, per
/// [`WINDOW`], in this process.
#[derive(Debug)]
pub(super) struct LocalCounters {
    counters: Mutex<Counters>,
}

impl LocalCounters {
    /// Empty counters.
    pub(super) fn new() -> Self {
        Self {
            counters: Mutex::new(Counters {
                total: Window {
                    start: Instant::now(),
                    count: 0,
                },
                per_ip: HashMap::new(),
            }),
        }
    }

    /// Whether a start from `ip` at `now` stays within `per_ip` and `total`; an allowed start is
    /// counted, a refused one is not.
    pub(super) fn allow_at(&self, ip: IpAddr, per_ip: u32, total: u32, now: Instant) -> bool {
        let mut c = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        let used = c.per_ip.get(&ip).map_or(0, |w| w.current(now));
        if c.total.current(now) >= total || used >= per_ip {
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
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn per_address_and_total_in_windows() {
        let l = LocalCounters::new();
        let t = Instant::now();
        let allow = |a: &str, at| l.allow_at(ip(a), 2, 3, at);
        assert!(allow("192.0.2.1", t) && allow("192.0.2.1", t));
        assert!(!allow("192.0.2.1", t), "per address");
        assert!(allow("192.0.2.2", t));
        assert!(!allow("192.0.2.3", t), "total");
        assert!(allow("192.0.2.1", t + WINDOW), "new window");
    }

    #[test]
    fn refusals_are_not_counted() {
        let l = LocalCounters::new();
        let t = Instant::now();
        assert!(l.allow_at(ip("192.0.2.1"), 1, 100, t));
        for _ in 0..50 {
            assert!(!l.allow_at(ip("192.0.2.1"), 1, 100, t));
        }
        let c = l.counters.lock().unwrap();
        assert_eq!(c.total.count, 1, "refused starts do not use up the total");
    }

    #[test]
    fn memory_is_bounded() {
        let l = LocalCounters::new();
        let t = Instant::now();
        for i in 0..(MAX_TRACKED as u32 + 100) {
            assert!(l.allow_at(IpAddr::V4(Ipv4Addr::from_bits(i)), 5, u32::MAX, t));
        }
        assert_eq!(l.counters.lock().unwrap().per_ip.len(), MAX_TRACKED);
        // Expired windows make room again.
        assert!(l.allow_at(ip("203.0.113.9"), 5, u32::MAX, t + WINDOW));
        assert_eq!(l.counters.lock().unwrap().per_ip.len(), 1);
    }
}
