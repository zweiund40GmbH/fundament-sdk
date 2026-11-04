//! Reading a status document of another app (`docs/protocol.md` §3.3). The answer is untrusted: it must
//! be JSON of the document shape with valid required fields (`app`, the levels, the names and kinds of
//! the list entries, RFC 3339 times, non-negative figures); anything else rejects the whole answer.
//! Unknown fields and missing optional parts are accepted (see [`super::document`]).
//!
//! Texts are only checked for their type: show them as data (encode for the output), never interpret
//! them. List sizes are bounded by the body limit of the client.

use super::document::{Offer, StatusDocument};
use chrono::DateTime;

/// The body is not a valid status document. Carries nothing of the body (a field name at most), so it
/// may be logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid status document ({0})")]
pub struct InvalidDocument(pub &'static str);

/// `Err` naming `field` unless `ok`.
fn check(ok: bool, field: &'static str) -> Result<(), InvalidDocument> {
    if ok {
        Ok(())
    } else {
        Err(InvalidDocument(field))
    }
}

/// Whether `t` is an RFC 3339 time.
fn is_time(t: &str) -> bool {
    DateTime::parse_from_rfc3339(t).is_ok()
}

/// Whether an optional time is absent or an RFC 3339 time.
fn is_optional_time(t: Option<&str>) -> bool {
    t.is_none_or(is_time)
}

/// Whether a required text field has content.
fn filled(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Checks one offer.
fn check_offer(o: &Offer) -> Result<(), InvalidDocument> {
    check(filled(&o.kind), "angebote.art")?;
    check(filled(&o.record_type), "angebote.typ")?;
    let rate = o.events_per_minute;
    check(
        rate.is_finite() && rate >= 0.0,
        "angebote.ereignisseProMinute",
    )
}

impl StatusDocument {
    /// Parses and checks the body of a status answer.
    pub fn from_json(body: &[u8]) -> Result<Self, InvalidDocument> {
        let doc: Self = serde_json::from_slice(body).map_err(|_| InvalidDocument("JSON"))?;
        check(filled(&doc.app), "app")?;
        check(doc.as_of.is_empty() || is_time(&doc.as_of), "stand")?;
        doc.offers.iter().try_for_each(check_offer)?;
        for p in &doc.pollers {
            check(filled(&p.name), "abrufer.name")?;
            check(filled(&p.kind), "abrufer.art")?;
            check(
                is_optional_time(p.last_poll.as_deref()),
                "abrufer.letzterAbruf",
            )?;
        }
        for s in &doc.sources {
            check(filled(&s.source), "bezieht.quelle")?;
            check(filled(&s.kind), "bezieht.art")?;
            check(
                is_optional_time(s.last_poll.as_deref()),
                "bezieht.letzterAbruf",
            )?;
        }
        for e in &doc.events {
            check(is_time(&e.time), "ereignisse.zeit")?;
            check(filled(&e.kind), "ereignisse.art")?;
        }
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Level;
    use serde_json::{Value, json};

    /// The example of `docs/protocol.md` §3.
    fn example() -> Value {
        json!({
          "app": "billing", "version": "1.4.0", "stand": "2026-11-04T10:00:00Z", "zustand": "ok",
          "hinweise": [ { "stufe": "warnung", "text": "3 Fehlversuche am Feed in 24 Stunden" } ],
          "angebote": [ { "art": "feed", "typ": "urn:example:billing:invoice", "letzterCursor": 18231,
                          "ereignisseProMinute": 0.4, "ereignisse24h": 512 } ],
          "abrufer":  [ { "name": "crm", "art": "feed", "letzterAbruf": "2026-11-04T09:59:30Z",
                          "cursor": 18200, "rueckstand": 31, "fehlversuche24h": null } ],
          "bezieht":  [ { "quelle": "crm", "art": "feed", "letzterAbruf": "2026-11-04T09:59:00Z",
                          "zustand": "ok", "text": null } ],
          "ereignisse": [ { "zeit": "2026-11-04T09:55:00Z", "art": "bezug", "stufe": "ok",
                            "text": "Feed von crm wieder erreichbar" } ]
        })
    }

    fn parse(v: &Value) -> Result<StatusDocument, InvalidDocument> {
        StatusDocument::from_json(v.to_string().as_bytes())
    }

    #[test]
    fn protocol_example_round_trips() {
        let doc = parse(&example()).unwrap();
        assert_eq!((doc.app.as_str(), doc.state), ("billing", Level::Ok));
        assert_eq!(doc.notes[0].level, Level::Warning);
        assert_eq!(doc.offers[0].last_cursor, 18231);
        assert_eq!(doc.pollers[0].failures_24h, None);
        assert_eq!(doc.sources[0].text, None);
        assert_eq!(serde_json::to_value(&doc).unwrap(), example());
    }

    #[test]
    fn minimal_document_and_unknown_fields() {
        let doc = parse(&json!({ "app": "crm", "zustand": "fehler", "neu": [1, 2] })).unwrap();
        assert_eq!(doc.state, Level::Error);
        assert!(doc.version.is_empty() && doc.as_of.is_empty());
        assert!(doc.notes.is_empty() && doc.offers.is_empty() && doc.pollers.is_empty());
        assert!(doc.sources.is_empty() && doc.events.is_empty());
        let nulls = json!({ "app": "crm", "zustand": "ok", "version": null, "stand": null,
            "hinweise": null, "angebote": null, "abrufer": null, "bezieht": null, "ereignisse": null });
        let doc = parse(&nulls).unwrap();
        assert!(doc.version.is_empty() && doc.as_of.is_empty() && doc.notes.is_empty());
        assert!(doc.offers.is_empty() && doc.pollers.is_empty() && doc.events.is_empty());
        let mut v = example();
        v["angebote"][0]["x"] = json!(true);
        v["abrufer"][0] = json!({ "name": "crm", "art": "feed", "rueckstand": 0 });
        v["bezieht"][0] = json!({ "quelle": "crm", "art": "feed", "zustand": "warnung" });
        let doc = parse(&v).unwrap();
        let (p, s) = (&doc.pollers[0], &doc.sources[0]);
        assert_eq!(
            (p.last_poll.as_deref(), p.cursor, p.backlog),
            (None, None, 0)
        );
        assert_eq!(p.failures_24h, None);
        assert_eq!((s.last_poll.as_deref(), s.text.as_deref()), (None, None));
    }

    #[test]
    fn invalid_required_fields_reject_the_document() {
        assert_eq!(
            StatusDocument::from_json(b"<html>"),
            Err(InvalidDocument("JSON"))
        );
        let cases: Vec<(&str, Value, &str)> = vec![
            ("/app", json!(""), "app"),
            ("/app", json!(7), "JSON"),
            ("/zustand", json!("unbekannt"), "JSON"),
            ("/stand", json!("gestern"), "stand"),
            ("/hinweise/0/stufe", json!("info"), "JSON"),
            ("/angebote/0/typ", json!(" "), "angebote.typ"),
            (
                "/angebote/0/ereignisseProMinute",
                json!(-1.0),
                "angebote.ereignisseProMinute",
            ),
            ("/abrufer/0/rueckstand", json!(-1), "JSON"),
            ("/abrufer/0/rueckstand", json!(null), "JSON"),
            ("/angebote/0/letzterCursor", json!(null), "JSON"),
            ("/angebote/0/ereignisseProMinute", json!(null), "JSON"),
            ("/angebote/0/ereignisse24h", json!(null), "JSON"),
            ("/zustand", json!(null), "JSON"),
            (
                "/abrufer/0/letzterAbruf",
                json!("gestern"),
                "abrufer.letzterAbruf",
            ),
            ("/bezieht/0/quelle", json!(""), "bezieht.quelle"),
            ("/bezieht/0/zustand", json!(null), "JSON"),
            ("/ereignisse/0/zeit", json!("2026-11-04"), "ereignisse.zeit"),
            ("/ereignisse/0/art", json!(""), "ereignisse.art"),
        ];
        for (path, value, field) in cases {
            let mut v = example();
            *v.pointer_mut(path).unwrap() = value;
            assert_eq!(parse(&v), Err(InvalidDocument(field)), "{path}");
        }
        for (path, field) in [
            ("", "app"),
            ("", "zustand"),
            ("/angebote/0", "letzterCursor"),
            ("/angebote/0", "ereignisseProMinute"),
            ("/angebote/0", "ereignisse24h"),
            ("/abrufer/0", "rueckstand"),
        ] {
            let mut v = example();
            v.pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert_eq!(parse(&v), Err(InvalidDocument("JSON")), "{path}/{field}");
        }
    }
}
