//! Building a feed page from what the store read (`docs/protocol.md` §2.2), without IO.

use crate::protocol::feed::{FeedEvent, FeedPage};

/// Builds the page for `since` from up to `limit + 1` events in cursor order: the extra event only
/// signals `hasMore` and is not sent.
///
/// `scanned_to` is the last assigned cursor, read **before** the events (see
/// [`super::FeedSourceStore::last_cursor`]). Every event up to it was then visible to the read, so when
/// there are no more events, none between the last item (or `since`) and `scanned_to` matches the type
/// filter: `nextCursor` moves on to it, and a consumer of rare types does not scan the same events again
/// on every poll. `nextCursor` never goes back behind `since`.
///
/// Tombstones never carry `data`. `gap` is always `false`: the stores of this crate keep the newest state
/// of every record, so `since=0` is always a full sync.
pub fn build_page(
    mut events: Vec<FeedEvent>,
    since: u64,
    limit: u32,
    scanned_to: Option<u64>,
) -> FeedPage {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = events.len() > limit;
    events.truncate(limit);
    for e in &mut events {
        if e.deleted {
            e.data = None;
        }
    }
    let last = events.last().map_or(since, |e| e.cursor).max(since);
    let next_cursor = match scanned_to {
        Some(to) if !has_more => last.max(to),
        _ => last,
    };
    FeedPage {
        items: events,
        next_cursor,
        has_more,
        gap: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::feed::parse_page;
    use serde_json::json;

    fn ev(cursor: u64, deleted: bool) -> FeedEvent {
        FeedEvent {
            cursor,
            id: Some(cursor.to_string()),
            event_type: "urn:example:crm:customer".into(),
            key: "0192f3a1-7c2e-7000-8000-00000000002a".into(),
            version: cursor,
            deleted,
            occurred_at: Some("2026-11-04T09:12:44Z".into()),
            data: Some(json!({ "name": "A" })),
        }
    }

    #[test]
    fn has_more_from_the_extra_event() {
        let p = build_page(vec![ev(5, false), ev(7, false), ev(9, false)], 3, 2, None);
        assert!(p.has_more && !p.gap);
        assert_eq!(p.items.len(), 2, "the extra event is not sent");
        assert_eq!(p.next_cursor, 7);
        let exact = build_page(vec![ev(5, false), ev(7, false)], 3, 2, None);
        assert!(!exact.has_more && exact.next_cursor == 7);
        let empty = build_page(vec![], 42, 100, None);
        assert_eq!((empty.next_cursor, empty.has_more), (42, false));
    }

    #[test]
    fn filtered_cursor_moves_past_scanned_events() {
        assert_eq!(build_page(vec![], 3, 100, Some(40)).next_cursor, 40);
        assert_eq!(
            build_page(vec![ev(5, false)], 3, 100, Some(40)).next_cursor,
            40
        );
        assert_eq!(
            build_page(vec![], 50, 100, Some(40)).next_cursor,
            50,
            "never back"
        );
        let more = build_page(vec![ev(5, false), ev(7, false)], 3, 1, Some(40));
        assert_eq!((more.next_cursor, more.has_more), (5, true));
    }

    #[test]
    fn wire_shape_is_a_valid_page() {
        let p = build_page(vec![ev(5, false), ev(6, true)], 0, 10, None);
        let body = serde_json::to_vec(&p).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["nextCursor"], 6);
        assert_eq!(
            (v["hasMore"].clone(), v["gap"].clone()),
            (json!(false), json!(false))
        );
        assert_eq!(v["items"][0]["type"], "urn:example:crm:customer");
        assert_eq!(v["items"][0]["occurredAt"], "2026-11-04T09:12:44Z");
        assert!(
            v["items"][1].get("data").is_none(),
            "tombstones carry no data"
        );
        assert_eq!(parse_page(&body, 0).unwrap(), p, "consumers accept it");
    }
}
