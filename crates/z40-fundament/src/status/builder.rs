//! Building a [`StatusDocument`] without IO: the app loads its figures and hands them to the builder,
//! which applies the caps of the protocol and derives the overall level (`zustand`).

use super::document::{Note, Offer, Poller, Source, StatusDocument, StatusEvent};
use crate::health::{SourceHealth, assess};
use crate::protocol::{Level, timestamp};
use chrono::{DateTime, Duration, Utc};

/// Highest reported backlog (`rueckstand`); larger values are reported as this value.
pub const BACKLOG_CAP: u64 = 10_000;
/// Most events in the document.
pub const MAX_EVENTS: usize = 100;

/// Events per minute from the count of the last 5 minutes, rounded to one decimal.
pub fn events_per_minute(last_5_minutes: u64) -> f64 {
    (last_5_minutes as f64 / 5.0 * 10.0).round() / 10.0
}

/// Collects the parts of a status document. The overall level is the worst of the sources and of the
/// notes added with [`StatusBuilder::note`]; [`StatusBuilder::advisory`] adds a note that does not
/// change it (e.g. a count of failed attempts).
#[derive(Debug, Clone)]
pub struct StatusBuilder {
    doc: StatusDocument,
    now: DateTime<Utc>,
}

impl StatusBuilder {
    /// A document of `app` in `version`, built at `now`.
    pub fn new(app: impl Into<String>, version: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            doc: StatusDocument {
                app: app.into(),
                version: version.into(),
                as_of: timestamp(now),
                state: Level::Ok,
                notes: Vec::new(),
                offers: Vec::new(),
                pollers: Vec::new(),
                sources: Vec::new(),
                events: Vec::new(),
            },
            now,
        }
    }

    /// Adds a note that raises the overall level to at least `level`.
    pub fn note(mut self, level: Level, text: impl Into<String>) -> Self {
        self.doc.state = self.doc.state.max(level);
        self.advisory(level, text)
    }

    /// Adds a note that does not change the overall level.
    pub fn advisory(mut self, level: Level, text: impl Into<String>) -> Self {
        self.doc.notes.push(Note {
            level,
            text: text.into(),
        });
        self
    }

    /// Adds an offer.
    pub fn offer(mut self, offer: Offer) -> Self {
        self.doc.offers.push(offer);
        self
    }

    /// Adds a poller; the backlog is capped at [`BACKLOG_CAP`].
    pub fn poller(mut self, mut poller: Poller) -> Self {
        poller.backlog = poller.backlog.min(BACKLOG_CAP);
        self.doc.pollers.push(poller);
        self
    }

    /// Adds a source; its level raises the overall level.
    pub fn source(mut self, source: Source) -> Self {
        self.doc.state = self.doc.state.max(source.state);
        self.doc.sources.push(source);
        self
    }

    /// Adds the feed `name` from its stored health, assessed at the build time with `stale_after`
    /// ([`crate::health::assess`]). A failing source also gets a note `Feed von <name>: <text>`.
    pub fn feed_source(mut self, name: &str, health: &SourceHealth, stale_after: Duration) -> Self {
        let a = assess(health, self.now, stale_after);
        if a.level != Level::Ok {
            let text = format!("Feed von {name}: {}", a.text.as_deref().unwrap_or_default());
            self = self.note(a.level, text);
        }
        self.source(Source {
            source: name.to_owned(),
            kind: "feed".to_owned(),
            last_poll: health.last_run.map(timestamp),
            state: a.level,
            text: a.text,
        })
    }

    /// Adds an event. Events are expected newest first; only the first [`MAX_EVENTS`] are kept.
    pub fn event(
        mut self,
        time: DateTime<Utc>,
        kind: impl Into<String>,
        level: Level,
        text: impl Into<String>,
    ) -> Self {
        if self.doc.events.len() < MAX_EVENTS {
            self.doc.events.push(StatusEvent {
                time: timestamp(time),
                kind: kind.into(),
                level,
                text: text.into(),
            });
        }
        self
    }

    /// The finished document.
    pub fn build(self) -> StatusDocument {
        self.doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::RunResult;
    use chrono::TimeZone;
    use serde_json::json;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, h, m, 0).unwrap()
    }

    #[test]
    fn empty_document_is_ok() {
        let v =
            serde_json::to_value(StatusBuilder::new("billing", "1.0.0", t(9, 0)).build()).unwrap();
        assert_eq!(v["app"], "billing");
        assert_eq!(v["stand"], "2026-11-04T09:00:00Z");
        assert_eq!(v["zustand"], "ok");
        for k in ["hinweise", "angebote", "abrufer", "bezieht", "ereignisse"] {
            assert_eq!(v[k], json!([]), "{k}");
        }
    }

    #[test]
    fn feed_source_health_and_level() {
        let ok = SourceHealth {
            last_run: Some(t(9, 0)),
            last_success: Some(t(9, 0)),
            last_result: Some(RunResult::Ok),
            text: None,
        };
        let s = Duration::minutes(15);
        let fine = StatusBuilder::new("a", "1", t(9, 5))
            .feed_source("crm", &ok, s)
            .build();
        assert_eq!(
            serde_json::to_value(&fine).unwrap()["bezieht"],
            json!([{ "quelle": "crm", "art": "feed", "letzterAbruf": "2026-11-04T09:00:00Z",
                     "zustand": "ok", "text": null }])
        );
        assert_eq!(fine.state, Level::Ok);
        let old = StatusBuilder::new("a", "1", t(9, 30))
            .feed_source("crm", &ok, s)
            .build();
        assert_eq!(old.state, Level::Error);
        assert!(
            old.notes[0]
                .text
                .starts_with("Feed von crm: kein erfolgreicher Abruf")
        );
    }

    #[test]
    fn notes_caps_and_wire_names() {
        let poller = Poller {
            name: "crm".into(),
            kind: "feed".into(),
            last_poll: None,
            cursor: Some(3),
            backlog: 12_345,
            failures_24h: None,
        };
        let offer = Offer {
            kind: "feed".into(),
            record_type: "urn:example:billing:invoice".into(),
            last_cursor: 9,
            events_per_minute: events_per_minute(2),
            events_24h: 7,
        };
        let mut b = StatusBuilder::new("a", "1", t(9, 0))
            .advisory(Level::Warning, "3 Fehlversuche")
            .poller(poller)
            .offer(offer);
        for _ in 0..(MAX_EVENTS + 5) {
            b = b.event(t(8, 0), "bezug", Level::Ok, "x");
        }
        let doc = b.build();
        assert_eq!(doc.state, Level::Ok, "advisories do not change the level");
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["abrufer"][0]["rueckstand"], BACKLOG_CAP);
        assert_eq!(v["abrufer"][0]["fehlversuche24h"], json!(null));
        assert_eq!(v["angebote"][0]["ereignisseProMinute"], 0.4);
        assert_eq!(v["angebote"][0]["letzterCursor"], 9);
        assert_eq!(v["ereignisse"].as_array().unwrap().len(), MAX_EVENTS);
        assert_eq!(v["ereignisse"][0]["zeit"], "2026-11-04T08:00:00Z");
        let warned = StatusBuilder::new("a", "1", t(9, 0))
            .note(Level::Warning, "w")
            .build();
        assert_eq!(warned.state, Level::Warning);
        let back: StatusDocument = serde_json::from_value(v).unwrap();
        assert_eq!(back, doc);
    }
}
