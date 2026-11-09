//! Small helpers for the tests of apps that use this crate (feature `test-util`; enable it in
//! `[dev-dependencies]` only): record keys, feed pages that pass [`parse_page`], and status documents
//! that pass [`StatusDocument::from_json`]. They build wire data only; serve it with your own fake
//! server or feed it to your mapping directly.
//!
//! [`parse_page`]: crate::protocol::feed::parse_page

use crate::protocol::feed::{FeedEvent, FeedPage};
use crate::protocol::status::StatusDocument;
use crate::protocol::{Level, timestamp};
use chrono::Utc;
use serde_json::Value;

/// A record key (canonical lower-case UUID) derived from `n`, e.g. `key(7)` →
/// `00000000-0000-7000-8000-000000000007`. Distinct `n` give distinct keys.
pub fn key(n: u64) -> String {
    format!("00000000-0000-7000-8000-{n:012x}")
}

/// Builds a consistent feed page: events get ascending cursors after `since`, `nextCursor` is the last
/// cursor (or `since` for an empty page), `gap` is `false`.
#[derive(Debug, Clone)]
pub struct FeedPageBuilder {
    since: u64,
    items: Vec<FeedEvent>,
    has_more: bool,
}

impl FeedPageBuilder {
    /// An empty page answering `since`.
    pub fn new(since: u64) -> Self {
        Self {
            since,
            items: Vec::new(),
            has_more: false,
        }
    }

    /// Adds the state `data` (a JSON object) of record `key` in `version`.
    pub fn state(self, record_type: &str, key: &str, version: u64, data: Value) -> Self {
        self.push(record_type, key, version, Some(data))
    }

    /// Adds a tombstone of record `key` in `version`.
    pub fn tombstone(self, record_type: &str, key: &str, version: u64) -> Self {
        self.push(record_type, key, version, None)
    }

    /// Sets `hasMore` (default `false`).
    pub fn has_more(mut self, has_more: bool) -> Self {
        self.has_more = has_more;
        self
    }

    /// The page.
    pub fn build(self) -> FeedPage {
        let next_cursor = self.items.last().map_or(self.since, |e| e.cursor);
        FeedPage {
            items: self.items,
            next_cursor,
            has_more: self.has_more,
            gap: false,
        }
    }

    /// The page as the JSON body of a feed answer.
    pub fn json(self) -> String {
        serde_json::to_string(&self.build()).unwrap_or_default()
    }

    /// Appends an event with the next cursor; `data` `None` is a tombstone.
    fn push(mut self, record_type: &str, key: &str, version: u64, data: Option<Value>) -> Self {
        let cursor = self.items.last().map_or(self.since, |e| e.cursor) + 1;
        self.items.push(FeedEvent {
            cursor,
            id: Some(cursor.to_string()),
            event_type: record_type.to_owned(),
            key: key.to_owned(),
            version,
            deleted: data.is_none(),
            occurred_at: Some(timestamp(Utc::now())),
            data,
        });
        self
    }
}

/// A minimal valid status document of `app` in `state` (version `1.0.0`, built now, no lists). Fill
/// the public fields for more.
pub fn status_document(app: &str, state: Level) -> StatusDocument {
    StatusDocument {
        app: app.to_owned(),
        version: "1.0.0".to_owned(),
        as_of: timestamp(Utc::now()),
        state,
        notes: Vec::new(),
        offers: Vec::new(),
        pollers: Vec::new(),
        sources: Vec::new(),
        events: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::feed::{is_uuid, parse_page};
    use serde_json::json;

    #[test]
    fn keys_are_uuids() {
        assert!(is_uuid(&key(0)) && is_uuid(&key(u64::MAX >> 16)));
        assert_ne!(key(1), key(2));
    }

    #[test]
    fn built_pages_pass_the_page_rules() {
        let t = "urn:example:crm:customer";
        let body = FeedPageBuilder::new(10)
            .state(t, &key(1), 1, json!({ "name": "A" }))
            .tombstone(t, &key(2), 3)
            .has_more(true)
            .json();
        let page = parse_page(body.as_bytes(), 10).unwrap();
        let cursors: Vec<u64> = page.items.iter().map(|e| e.cursor).collect();
        assert_eq!(
            (cursors, page.next_cursor, page.has_more),
            (vec![11, 12], 12, true)
        );
        assert!(page.items[1].deleted && page.items[1].data.is_none());
        let empty = FeedPageBuilder::new(5).build();
        assert_eq!((empty.next_cursor, empty.items.len()), (5, 0));
    }

    #[test]
    fn status_documents_pass_the_reader() {
        let doc = status_document("crm", Level::Warning);
        let body = serde_json::to_vec(&doc).unwrap();
        assert_eq!(StatusDocument::from_json(&body).unwrap(), doc);
    }
}
