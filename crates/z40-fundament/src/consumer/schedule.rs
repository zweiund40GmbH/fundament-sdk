//! Scheduling help for the app's worker: the name of the slot a run belongs to. Several workers that
//! enqueue "the run of this slot" at most once (e.g. with a unique slot column) start one run per
//! interval, also across restarts.

use chrono::{DateTime, Utc};
use std::time::Duration;

/// The slot of `now` for runs every `interval`: the start of the interval as `YYYY-MM-DDTHH:MM:SSZ`.
/// Computed from UTC on purpose – when clocks go back in local time, local times repeat and would hit
/// slots that already exist, so runs would be skipped for an hour. An interval below one second counts
/// as one second.
pub fn slot_for(now: DateTime<Utc>, interval: Duration) -> String {
    let secs = i64::try_from(interval.as_secs().max(1)).unwrap_or(i64::MAX);
    let t = now.timestamp();
    let start = t - t.rem_euclid(secs);
    DateTime::from_timestamp(start, 0).map_or_else(
        || start.to_string(),
        |d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(d: u32, h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, d, h, m, s).unwrap()
    }

    #[test]
    fn slots_follow_the_interval() {
        let secs = Duration::from_secs;
        assert_eq!(slot_for(at(4, 9, 0, 59), secs(60)), "2026-10-04T09:00:00Z");
        assert_eq!(slot_for(at(4, 9, 1, 0), secs(60)), "2026-10-04T09:01:00Z");
        assert_eq!(slot_for(at(4, 9, 1, 25), secs(30)), "2026-10-04T09:01:00Z");
        assert_eq!(
            slot_for(at(4, 9, 59, 59), secs(3600)),
            "2026-10-04T09:00:00Z"
        );
        assert_eq!(
            slot_for(at(4, 9, 0, 7), Duration::ZERO),
            "2026-10-04T09:00:07Z"
        );
    }

    #[test]
    fn daylight_saving_end_gives_distinct_slots() {
        // 2026-10-25 00:30Z and 01:30Z are both 02:30 local time in Central Europe.
        let a = slot_for(at(25, 0, 30, 0), Duration::from_secs(60));
        let b = slot_for(at(25, 1, 30, 0), Duration::from_secs(60));
        assert_ne!(a, b);
        assert_eq!(a, "2026-10-25T00:30:00Z");
    }
}
