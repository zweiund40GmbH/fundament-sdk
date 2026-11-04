//! The source side of the status document (`docs/protocol.md` §3): `angebote` per record type, `abrufer`
//! per configured feed key, and the failed attempts of 24 hours as a note. Failed attempts cannot be
//! attributed to a consumer (a wrong key names nobody), so `fehlversuche24h` is always `null`.

use super::{FeedSourceStore, OfferFigures, PollRecord, SourceStatusStore};
use crate::StoreError;
use crate::credentials::KeyList;
use crate::protocol::{Level, timestamp};
use crate::status::{BACKLOG_CAP, Offer, Poller, StatusBuilder, events_per_minute};
use chrono::{DateTime, Duration, Utc};

/// One offer per known type, in the order of `known`, with zeros for types without figures.
pub fn offers(known: &[String], figures: &[OfferFigures]) -> Vec<Offer> {
    known
        .iter()
        .map(|t| {
            let f = figures
                .iter()
                .find(|f| &f.record_type == t)
                .cloned()
                .unwrap_or_default();
            Offer {
                kind: "feed".to_owned(),
                record_type: t.clone(),
                last_cursor: f.last_cursor,
                events_per_minute: events_per_minute(f.last_5_minutes),
                events_24h: f.last_24_hours,
            }
        })
        .collect()
}

/// The poller `name` from its last poll (`None` = never polled) and its backlog.
pub fn poller(name: &str, poll: Option<&PollRecord>, backlog: u64) -> Poller {
    Poller {
        name: name.to_owned(),
        kind: "feed".to_owned(),
        last_poll: poll.map(|p| timestamp(p.last_poll)),
        cursor: poll.map(|p| p.cursor),
        backlog: backlog.min(BACKLOG_CAP),
        failures_24h: None,
    }
}

/// The note on `n` failed feed attempts in 24 hours (`None` for 0), e.g.
/// `3 Fehlversuche am Feed in 24 Stunden`.
pub fn failed_attempts_note(n: u64) -> Option<String> {
    let noun = if n == 1 {
        "Fehlversuch"
    } else {
        "Fehlversuche"
    };
    (n > 0).then(|| format!("{n} {noun} am Feed in 24 Stunden"))
}

/// Adds the source figures of `store` to `builder` at `now`: an offer per known type, a poller per
/// configured feed key of `clients` (also those that never polled, with the backlog from cursor 0), and
/// the failed attempts of 24 hours as an advisory of level `warnung` that does not change `zustand`.
pub async fn source_status<S>(
    builder: StatusBuilder,
    store: &S,
    clients: &KeyList,
    now: DateTime<Utc>,
) -> Result<StatusBuilder, StoreError>
where
    S: FeedSourceStore + SourceStatusStore,
{
    let figures = store.offer_figures(now).await?;
    let mut b = offers(store.known_types(), &figures)
        .into_iter()
        .fold(builder, StatusBuilder::offer);
    let polls = store.polls().await?;
    for name in clients.names() {
        let poll = polls.iter().find(|p| p.consumer == name);
        let backlog = store
            .backlog(poll.map_or(0, |p| p.cursor), BACKLOG_CAP)
            .await?;
        b = b.poller(poller(name, poll, backlog));
    }
    let failed = store.failed_attempts(now - Duration::hours(24)).await?;
    if let Some(text) = failed_attempts_note(failed) {
        b = b.advisory(Level::Warning, text);
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn offers_in_known_order_with_zeros() {
        let known = [
            "urn:example:crm:customer".to_owned(),
            "urn:example:crm:site".to_owned(),
        ];
        let figures = [OfferFigures {
            record_type: known[1].clone(),
            last_cursor: 9,
            last_5_minutes: 2,
            last_24_hours: 7,
        }];
        let o = offers(&known, &figures);
        assert_eq!(o.len(), 2);
        assert_eq!((o[0].last_cursor, o[0].events_24h), (0, 0));
        assert_eq!((o[1].last_cursor, o[1].events_per_minute), (9, 0.4));
    }

    #[test]
    fn pollers_cap_backlog_and_never_attribute_failures() {
        let at = Utc.with_ymd_and_hms(2026, 11, 4, 9, 0, 0).unwrap();
        let rec = PollRecord {
            consumer: "crm".into(),
            last_poll: at,
            cursor: 18,
        };
        let p = poller("crm", Some(&rec), 12_345);
        assert_eq!(p.backlog, BACKLOG_CAP);
        assert_eq!(p.last_poll.as_deref(), Some("2026-11-04T09:00:00Z"));
        assert_eq!((p.cursor, p.failures_24h), (Some(18), None));
        let never = poller("billing", None, 3);
        assert_eq!(
            (never.last_poll, never.cursor, never.backlog),
            (None, None, 3)
        );
    }

    #[test]
    fn failure_note() {
        assert_eq!(failed_attempts_note(0), None);
        assert_eq!(
            failed_attempts_note(1).as_deref(),
            Some("1 Fehlversuch am Feed in 24 Stunden")
        );
        assert_eq!(
            failed_attempts_note(3).as_deref(),
            Some("3 Fehlversuche am Feed in 24 Stunden")
        );
    }
}
