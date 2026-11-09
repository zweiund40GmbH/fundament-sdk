//! Wire format of version 1 of the protocol (`docs/protocol.md`): paths, the change feed page and the
//! status level. Pure types and rules without IO, always compiled; the features build on them.
//!
//! * [`feed`] – feed events and pages, page validation, version rule
//! * [`status`] – the status document and its lenient, checked reading

pub mod feed;
pub mod status;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// Version of the protocol described in `docs/protocol.md`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Path of the change feed of a source app.
pub const EVENTS_PATH: &str = "/fundament/v1/events";
/// Path of the status document of an app.
pub const STATUS_PATH: &str = "/fundament/v1/status";
/// Largest `limit` a feed request may ask for.
pub const MAX_PAGE_LIMIT: u32 = 500;

/// Level of a status, note or event. Serialized as `ok`, `warnung`, `fehler` (wire format of
/// version 1). Ordered from best to worst, so the overall state is the maximum.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Level {
    /// Fine (`ok`).
    #[default]
    #[serde(rename = "ok")]
    Ok,
    /// Needs a look (`warnung`).
    #[serde(rename = "warnung")]
    Warning,
    /// Broken (`fehler`).
    #[serde(rename = "fehler")]
    Error,
}

impl Level {
    /// The wire code (`ok` | `warnung` | `fehler`), e.g. for storing in a database column.
    pub fn code(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warning => "warnung",
            Level::Error => "fehler",
        }
    }

    /// Level of a stored code; anything unknown reads as [`Level::Error`] (the safe reading).
    pub fn from_code(code: &str) -> Self {
        match code {
            "ok" => Level::Ok,
            "warnung" => Level::Warning,
            _ => Level::Error,
        }
    }
}

/// A UTC time in the wire form: RFC 3339 with seconds and `Z`, e.g. `2026-11-04T09:00:00Z`.
pub fn timestamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn levels_and_times() {
        assert_eq!(
            serde_json::to_string(&Level::Warning).unwrap(),
            "\"warnung\""
        );
        assert_eq!(
            serde_json::from_str::<Level>("\"fehler\"").unwrap(),
            Level::Error
        );
        for l in [Level::Ok, Level::Warning, Level::Error] {
            assert_eq!(Level::from_code(l.code()), l);
        }
        assert_eq!(Level::from_code("?"), Level::Error);
        assert!(Level::Ok < Level::Warning && Level::Warning < Level::Error);
        let t = Utc.with_ymd_and_hms(2026, 11, 4, 9, 0, 5).unwrap();
        assert_eq!(timestamp(t), "2026-11-04T09:00:05Z");
    }
}
