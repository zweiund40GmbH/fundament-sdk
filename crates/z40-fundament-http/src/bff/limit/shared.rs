//! [`LoginLimitStore`]: counters of the login limit shared by all instances of the app (e.g.
//! `bff-mysql`), so N instances allow the configured limit, not N times it.
//!
//! Windows are aligned to the clock (each starts at a full minute, UTC), so every instance counts into
//! the same window. A start first counts against its client address; only if that stays within the
//! limit it counts against the total. A refused start may thus raise the counter that refused it, but
//! that counter already refuses everything for the rest of its window, so the outcome equals the
//! in-process counters (and the address counter of a start refused by the total is not visible either:
//! the total refuses every start of that window).

use super::WINDOW;
use crate::bff::store::BoxFuture;
use crate::bff::token::TokenHash;
use chrono::{DateTime, TimeDelta, Utc};
use std::future::Future;
use std::net::IpAddr;
use z40_fundament::StoreError;

/// Counters of the login limit shared by the instances of an app ([`super::super::BffBuilder::login_limit_store`]).
///
/// A key is the SHA-256 of `total` or of the client address (IPv6 reduced to its /64), never the
/// address itself.
pub trait LoginLimitStore: Send + Sync + 'static {
    /// Adds one to counter `key` in the window `window_start` (a full minute, UTC) and returns the count
    /// after it. Must be atomic: of concurrent calls each gets a different count. The counter is no
    /// longer needed after `window_end` (for the store's cleanup).
    fn increment(
        &self,
        key: &TokenHash,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;
}

/// [`LoginLimitStore`] behind `dyn`.
pub(crate) trait DynLimitStore: Send + Sync {
    fn increment<'a>(
        &'a self,
        key: &'a TokenHash,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> BoxFuture<'a, u64>;
}

impl<S: LoginLimitStore> DynLimitStore for S {
    fn increment<'a>(
        &'a self,
        key: &'a TokenHash,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> BoxFuture<'a, u64> {
        Box::pin(LoginLimitStore::increment(self, key, start, end))
    }
}

/// The start of the window of `now`: the full minute (UTC) at or before it.
pub(super) fn window_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let secs = now.timestamp();
    let start = secs - secs.rem_euclid(WINDOW.as_secs() as i64);
    DateTime::from_timestamp(start, 0).unwrap_or(now)
}

/// Whether a start from `ip` (already reduced to its /64) at `now` stays within `per_ip` and `total`
/// in the shared `store`; counts it (see the module docs).
pub(super) async fn allow_at(
    store: &dyn DynLimitStore,
    ip: IpAddr,
    per_ip: u32,
    total: u32,
    now: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let start = window_start(now);
    let end = start + TimeDelta::seconds(WINDOW.as_secs() as i64);
    let address = TokenHash::of(&format!("ip:{ip}"));
    if store.increment(&address, start, end).await? > u64::from(per_ip) {
        return Ok(false);
    }
    let all = TokenHash::of("total");
    Ok(store.increment(&all, start, end).await? <= u64::from(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Counter key and window start.
    type Slot = ([u8; 32], DateTime<Utc>);

    /// Counters in memory, failing on demand.
    #[derive(Default)]
    struct Memory {
        counts: Mutex<HashMap<Slot, u64>>,
        fail: bool,
    }

    impl LoginLimitStore for Memory {
        async fn increment(
            &self,
            key: &TokenHash,
            start: DateTime<Utc>,
            end: DateTime<Utc>,
        ) -> Result<u64, StoreError> {
            assert_eq!(end - start, TimeDelta::seconds(60));
            if self.fail {
                return Err(StoreError::new(std::io::Error::other("down")));
            }
            let mut c = self.counts.lock().unwrap();
            let n = c.entry((*key.as_bytes(), start)).or_default();
            *n += 1;
            Ok(*n)
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn windows_are_full_minutes() {
        assert_eq!(window_start(at(120)), at(120));
        assert_eq!(window_start(at(179)), at(120));
        let t = at(1_800_000_059) + TimeDelta::milliseconds(999);
        assert_eq!(window_start(t), at(1_800_000_000));
    }

    #[tokio::test]
    async fn per_address_and_total_like_the_local_counters() {
        let m = Memory::default();
        let allow = |a: &'static str, t| allow_at(&m, ip(a), 2, 3, t);
        let t = at(1_800_000_010);
        assert!(allow("192.0.2.1", t).await.unwrap() && allow("192.0.2.1", t).await.unwrap());
        assert!(!allow("192.0.2.1", t).await.unwrap(), "per address");
        assert!(allow("192.0.2.2", t).await.unwrap());
        assert!(!allow("192.0.2.3", t).await.unwrap(), "total");
        assert!(
            !allow("192.0.2.4", at(1_800_000_059)).await.unwrap(),
            "same window"
        );
        assert!(
            allow("192.0.2.1", at(1_800_000_060)).await.unwrap(),
            "next window"
        );
        let keys = m.counts.lock().unwrap().len();
        assert_eq!(keys, 5 + 2, "addresses and totals of two windows");
    }

    #[tokio::test]
    async fn refused_per_address_does_not_use_up_the_total() {
        let m = Memory::default();
        let t = at(1_800_000_000);
        for _ in 0..20 {
            allow_at(&m, ip("192.0.2.1"), 1, 3, t).await.unwrap();
        }
        assert!(allow_at(&m, ip("192.0.2.2"), 1, 3, t).await.unwrap());
        assert!(allow_at(&m, ip("192.0.2.3"), 1, 3, t).await.unwrap());
    }

    #[tokio::test]
    async fn store_errors_are_passed_on() {
        let m = Memory {
            fail: true,
            ..Memory::default()
        };
        assert!(allow_at(&m, ip("192.0.2.1"), 1, 3, at(0)).await.is_err());
    }
}
