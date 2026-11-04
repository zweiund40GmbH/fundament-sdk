//! The change feed (`GET /fundament/v1/events`, `docs/protocol.md`): one event carries the complete
//! current state of a record and its version, so duplicate, late or reordered events are harmless.
//! This module parses and checks a page and holds the version rule; fetching and storing live in
//! the features.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One event of the feed. Unknown fields are ignored (sources may add fields at any time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedEvent {
    /// Position in the feed (assigned by the source after commit, never reused).
    pub cursor: u64,
    /// Unique id of the event, for de-duplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Record type URN (a type of record, not an action), e.g. `urn:example:crm:customer`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Stable id of the record (UUID).
    pub key: String,
    /// Version of the record; strictly increasing per record.
    pub version: u64,
    /// Tombstone: the record was deleted, `data` is absent.
    #[serde(default)]
    pub deleted: bool,
    /// When the change happened (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    /// Complete current state of the record (absent for tombstones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl FeedEvent {
    /// The state of the record, or `None` for a tombstone (a tombstone's `data` is never used).
    pub fn state(&self) -> Option<&Value> {
        self.data.as_ref().filter(|_| !self.deleted)
    }
}

/// One page of the feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedPage {
    /// The events in cursor order.
    pub items: Vec<FeedEvent>,
    /// Cursor to continue from (`since` of the next request).
    pub next_cursor: u64,
    /// More events after `next_cursor`.
    pub has_more: bool,
    /// Events after `since` were already purged: the consumer must read again from 0.
    pub gap: bool,
}

/// Why a page was rejected. Carries nothing of the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// Not the feed format, or inconsistent (cursor going back, invalid key, state without data).
    #[error("invalid feed page")]
    Invalid,
}

/// Parses a feed page and checks it against `since`:
///
/// * `nextCursor` never goes back (except with `gap`),
/// * `nextCursor` moves on while `hasMore` (else the consumer would fetch the same page forever),
/// * the cursors of the events are strictly ascending, not greater than `nextCursor` and – except
///   with `gap` – greater than `since` (a page that repeats or reorders events would make the stored
///   cursor lie about what was applied),
/// * every key is a UUID,
/// * every event that is not a tombstone carries `data` (a missing state is not a silent delete).
pub fn parse_page(body: &[u8], since: u64) -> Result<FeedPage, PageError> {
    let page: FeedPage = serde_json::from_slice(body).map_err(|_| PageError::Invalid)?;
    if !page.gap && (page.next_cursor < since || (page.has_more && page.next_cursor == since)) {
        return Err(PageError::Invalid);
    }
    let floor = if page.gap { 0 } else { since };
    let ascending = page
        .items
        .iter()
        .try_fold(floor, |prev, e| (e.cursor > prev).then_some(e.cursor))
        .is_some_and(|last| last <= page.next_cursor || page.items.is_empty());
    if !ascending {
        return Err(PageError::Invalid);
    }
    let broken = |e: &FeedEvent| !is_uuid(&e.key) || (!e.deleted && e.data.is_none());
    if page.items.iter().any(broken) {
        return Err(PageError::Invalid);
    }
    Ok(page)
}

/// Whether `s` is a UUID in its canonical 36-character form (any case).
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Whether an event with version `incoming` replaces the stored state with version `stored`
/// (`None` = not stored yet). Only strictly higher versions win, so applying a page twice, or an
/// older event after a newer one, changes nothing.
pub fn should_apply(stored: Option<u64>, incoming: u64) -> bool {
    stored.is_none_or(|v| incoming > v)
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
