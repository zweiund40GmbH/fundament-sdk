//! Health of a source an app reads from (a feed it consumes): the stored result of the last runs,
//! the rule when the source counts as failing, and the event written when the result changes.
//! Pure functions; storage is the app's (or `replica-mysql`'s) business.
//!
//! Texts are fixed and never contain response bodies, keys or record contents. They are German,
//! like all human-readable texts of version 1 of the status format (`docs/protocol.md`).

use crate::protocol::Level;
use chrono::{DateTime, Duration, Utc};

/// Default age after which the last successful run counts as too old.
pub const DEFAULT_STALE_AFTER: Duration = Duration::minutes(15);
/// Text while no run has finished yet.
pub const TEXT_NEVER: &str = "noch kein Abruf";
/// Text of a failed run that stored no text.
pub const TEXT_FAILED: &str = "Abruf fehlgeschlagen";

/// Result of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunResult {
    /// The run reached the end of the feed (or its page budget) without error.
    Ok,
    /// The run stopped with an error.
    Failed,
}

impl RunResult {
    /// Stored code (`ok` | `fehler`).
    pub fn code(self) -> &'static str {
        match self {
            RunResult::Ok => "ok",
            RunResult::Failed => "fehler",
        }
    }

    /// Result of a stored code; anything but `ok` reads as failed.
    pub fn from_code(code: &str) -> Self {
        if code == "ok" {
            RunResult::Ok
        } else {
            RunResult::Failed
        }
    }
}

/// The stored health of a source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceHealth {
    /// End of the last run.
    pub last_run: Option<DateTime<Utc>>,
    /// End of the last successful run.
    pub last_success: Option<DateTime<Utc>>,
    /// Result of the last run; `None` = never ran.
    pub last_result: Option<RunResult>,
    /// Fixed error text of the last run.
    pub text: Option<String>,
}

/// Level and text of a source at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// `ok` or `fehler`.
    pub level: Level,
    /// Why it is not ok; `None` when ok.
    pub text: Option<String>,
}

/// Text while the last success is older than `stale_after`.
pub fn stale_text(stale_after: Duration) -> String {
    format!(
        "kein erfolgreicher Abruf seit über {} Minuten",
        stale_after.num_minutes()
    )
}

/// Health of the source at `now`: error when it never ran, when the last run failed, or when the
/// last successful run is older than `stale_after`; otherwise ok.
pub fn assess(health: &SourceHealth, now: DateTime<Utc>, stale_after: Duration) -> Assessment {
    let error = |text: String| Assessment {
        level: Level::Error,
        text: Some(text),
    };
    match health.last_result {
        None => return error(TEXT_NEVER.to_owned()),
        Some(RunResult::Failed) => {
            return error(
                health
                    .text
                    .clone()
                    .unwrap_or_else(|| TEXT_FAILED.to_owned()),
            );
        }
        Some(RunResult::Ok) => {}
    }
    match health.last_success {
        Some(t) if now - t <= stale_after => Assessment {
            level: Level::Ok,
            text: None,
        },
        _ => error(stale_text(stale_after)),
    }
}

/// The event to record when a run of `source` ends with `error` (`None` = success) after a run
/// with result `previous` (`None` = first run): a failure after success or as first result, and the
/// recovery after a failure. An unchanged result gives `None`, so a long outage is one event.
pub fn transition(
    source: &str,
    previous: Option<RunResult>,
    error: Option<&str>,
) -> Option<(Level, String)> {
    let was_failed = previous == Some(RunResult::Failed);
    match error {
        Some(text) if !was_failed => Some((Level::Error, format!("Feed von {source}: {text}"))),
        None if was_failed => Some((Level::Ok, format!("Feed von {source} wieder erreichbar"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, h, m, 0).unwrap()
    }

    #[test]
    fn health_rule() {
        let ok = SourceHealth {
            last_run: Some(t(9, 0)),
            last_success: Some(t(9, 0)),
            last_result: Some(RunResult::Ok),
            text: None,
        };
        let s = DEFAULT_STALE_AFTER;
        assert_eq!(assess(&ok, t(9, 15), s).level, Level::Ok);
        let stale = assess(&ok, t(9, 16), s);
        assert_eq!(stale.level, Level::Error);
        assert_eq!(
            stale.text.as_deref(),
            Some("kein erfolgreicher Abruf seit über 15 Minuten")
        );
        let failed = SourceHealth {
            last_result: Some(RunResult::Failed),
            text: Some("HTTP 401".into()),
            ..ok.clone()
        };
        assert_eq!(
            assess(&failed, t(9, 1), s).text.as_deref(),
            Some("HTTP 401")
        );
        let failed_no_text = SourceHealth {
            text: None,
            ..failed
        };
        assert_eq!(
            assess(&failed_no_text, t(9, 1), s).text.as_deref(),
            Some(TEXT_FAILED)
        );
        let never = assess(&SourceHealth::default(), t(9, 0), s);
        assert_eq!(
            (never.level, never.text.as_deref()),
            (Level::Error, Some(TEXT_NEVER))
        );
    }

    #[test]
    fn events_only_on_change() {
        let e = Some("nicht erreichbar");
        assert_eq!(
            transition("crm", None, e),
            Some((Level::Error, "Feed von crm: nicht erreichbar".into()))
        );
        assert!(transition("crm", Some(RunResult::Ok), e).is_some());
        assert_eq!(transition("crm", Some(RunResult::Failed), e), None);
        assert_eq!(transition("crm", None, None), None);
        assert_eq!(transition("crm", Some(RunResult::Ok), None), None);
        assert_eq!(
            transition("crm", Some(RunResult::Failed), None),
            Some((Level::Ok, "Feed von crm wieder erreichbar".into()))
        );
    }

    #[test]
    fn result_codes() {
        assert_eq!(RunResult::from_code(RunResult::Ok.code()), RunResult::Ok);
        assert_eq!(RunResult::from_code("fehler"), RunResult::Failed);
        assert_eq!(RunResult::from_code("?"), RunResult::Failed);
    }
}
