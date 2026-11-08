//! Session lifetime: an **idle** timeout (no request for that long ends the session) and an
//! **absolute** timeout (the session ends that long after the login, however active it is). The app
//! decides on roles at login; the absolute timeout bounds how long a removed role stays usable when no
//! back-channel logout arrives.
//!
//! A session stores its login time, the time of its last recorded use and the resulting end
//! ([`Timeouts::expires_at`]); the store needs that end for its cleanup. The last use is written at most
//! every [`Timeouts::touch_interval`], not on every request, so the idle timeout is exact to that
//! interval.

use chrono::{DateTime, TimeDelta, Utc};

/// Idle and absolute timeout of sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    idle: TimeDelta,
    absolute: TimeDelta,
}

/// Longest gap between two writes of the last use.
const MAX_TOUCH_INTERVAL: TimeDelta = TimeDelta::seconds(60);

impl Timeouts {
    /// Timeouts of `idle_secs` and `absolute_secs` (both at least 1); an idle timeout longer than the
    /// absolute one is cut to it.
    pub fn from_secs(idle_secs: u32, absolute_secs: u32) -> Self {
        let absolute = TimeDelta::seconds(i64::from(absolute_secs.max(1)));
        let idle = TimeDelta::seconds(i64::from(idle_secs.max(1))).min(absolute);
        Self { idle, absolute }
    }

    /// The idle timeout.
    pub fn idle(&self) -> TimeDelta {
        self.idle
    }

    /// The absolute timeout.
    pub fn absolute(&self) -> TimeDelta {
        self.absolute
    }

    /// The end of a session that started at `created` and was last used at `last_seen`: whichever
    /// timeout comes first.
    pub fn expires_at(&self, created: DateTime<Utc>, last_seen: DateTime<Utc>) -> DateTime<Utc> {
        (last_seen + self.idle).min(created + self.absolute)
    }

    /// Whether the session is still valid at `now` under these timeouts. Judged from the login time and
    /// the last use (not only from a stored end), so shortened timeouts apply to running sessions too.
    pub fn is_valid(
        &self,
        created: DateTime<Utc>,
        last_seen: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> bool {
        now < self.expires_at(created, last_seen)
    }

    /// How often the last use is written: a tenth of the idle timeout, at most a minute.
    pub fn touch_interval(&self) -> TimeDelta {
        (self.idle / 10).min(MAX_TOUCH_INTERVAL)
    }

    /// Whether a request at `now` should record the use of a session last recorded at `last_seen`.
    pub fn touch_due(&self, last_seen: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        now - last_seen >= self.touch_interval()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn idle_and_absolute_timeout() {
        let t = Timeouts::from_secs(600, 3600);
        assert!(t.is_valid(at(0), at(0), at(599)));
        assert!(!t.is_valid(at(0), at(0), at(600)), "idle reached");
        assert!(t.is_valid(at(0), at(3000), at(3599)));
        assert!(!t.is_valid(at(0), at(3500), at(3600)), "absolute reached");
        assert_eq!(t.expires_at(at(0), at(100)), at(700));
        assert_eq!(t.expires_at(at(0), at(3500)), at(3600));
    }

    #[test]
    fn idle_is_cut_to_absolute_and_zero_is_refused() {
        let t = Timeouts::from_secs(7200, 3600);
        assert_eq!(t.idle(), t.absolute());
        let z = Timeouts::from_secs(0, 0);
        assert_eq!(
            (z.idle(), z.absolute()),
            (TimeDelta::seconds(1), TimeDelta::seconds(1))
        );
    }

    #[test]
    fn use_is_recorded_at_most_every_interval() {
        let t = Timeouts::from_secs(7200, 36000);
        assert_eq!(t.touch_interval(), TimeDelta::seconds(60));
        assert!(!t.touch_due(at(0), at(59)));
        assert!(t.touch_due(at(0), at(60)));
        let short = Timeouts::from_secs(120, 3600);
        assert_eq!(short.touch_interval(), TimeDelta::seconds(12));
    }

    #[test]
    fn clock_going_backwards_keeps_the_session_bounded() {
        let t = Timeouts::from_secs(600, 3600);
        // A last use "in the future" (clock skew between instances) never extends past the absolute end.
        assert_eq!(t.expires_at(at(0), at(5000)), at(3600));
        assert!(!t.touch_due(at(100), at(50)));
    }
}
