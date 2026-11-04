use super::*;
use serde_json::json;

const K: &str = "0192f39e-0000-7000-8000-000000000001";
const T: &str = "urn:example:crm:customer";

fn body(v: Value) -> Vec<u8> {
    v.to_string().into_bytes()
}

fn event(deleted: bool, data: Value) -> Value {
    json!({ "cursor": 1, "type": T, "key": K, "version": 1, "deleted": deleted, "data": data })
}

#[test]
fn valid_page_with_unknown_fields() {
    let ok = body(json!({
        "items": [{ "cursor": 5, "id": "e1", "type": T, "key": K, "version": 2, "deleted": false,
                    "occurredAt": "2026-11-04T09:00:00Z", "data": { "name": "A" }, "new": 1 }],
        "nextCursor": 5, "hasMore": false, "gap": false, "extra": true
    }));
    let p = parse_page(&ok, 3).unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.next_cursor, 5);
    assert_eq!(p.items[0].event_type, T);
    assert_eq!(
        p.items[0].occurred_at.as_deref(),
        Some("2026-11-04T09:00:00Z")
    );
    assert_eq!(p.items[0].state(), Some(&json!({ "name": "A" })));
}

#[test]
fn cursor_rules() {
    let page = |next: u64, more: bool, gap: bool| {
        body(json!({ "items": [], "nextCursor": next, "hasMore": more, "gap": gap }))
    };
    assert_eq!(
        parse_page(&page(5, false, false), 6),
        Err(PageError::Invalid),
        "going back"
    );
    assert!(
        parse_page(&page(0, true, true), 9).unwrap().gap,
        "allowed with gap"
    );
    assert!(
        parse_page(&page(4, false, false), 4).is_ok(),
        "last page may end where it started"
    );
    assert_eq!(
        parse_page(&page(4, true, false), 4),
        Err(PageError::Invalid),
        "more announced but the cursor does not move"
    );
}

#[test]
fn tombstones_and_missing_state() {
    let page =
        |e: Value| body(json!({ "items": [e], "nextCursor": 5, "hasMore": false, "gap": false }));
    let tomb = parse_page(&page(event(true, Value::Null)), 4).unwrap();
    assert_eq!(tomb.items[0].state(), None);
    let tomb_with_data = parse_page(&page(event(true, json!({ "name": "A" }))), 4).unwrap();
    assert_eq!(
        tomb_with_data.items[0].state(),
        None,
        "a tombstone's data is never used"
    );
    assert_eq!(
        parse_page(&page(event(false, Value::Null)), 4),
        Err(PageError::Invalid)
    );
}

#[test]
fn malformed_pages() {
    for bad in [
        b"<html>".to_vec(),
        body(json!({ "items": [] })),
        body(
            json!({ "items": [{ "cursor": 1, "type": "t", "key": "nope", "version": 1, "data": {} }],
                     "nextCursor": 1, "hasMore": false, "gap": false }),
        ),
    ] {
        assert_eq!(parse_page(&bad, 0), Err(PageError::Invalid));
    }
}

#[test]
fn uuid_form() {
    assert!(is_uuid(K));
    assert!(is_uuid(&K.to_ascii_uppercase()));
    assert!(!is_uuid("0192f39e00007000800000000000000001"));
    assert!(!is_uuid("0192f39e-0000-7000-8000-00000000000g"));
    assert!(!is_uuid(""));
}

#[test]
fn version_rule() {
    assert!(should_apply(None, 1));
    assert!(should_apply(Some(1), 2));
    assert!(
        !should_apply(Some(2), 2),
        "same version twice changes nothing"
    );
    assert!(
        !should_apply(Some(3), 2),
        "older event after a newer one is dropped"
    );
}

#[test]
fn page_round_trip() {
    let p = FeedPage {
        items: vec![FeedEvent {
            cursor: 3,
            id: None,
            event_type: T.into(),
            key: K.into(),
            version: 1,
            deleted: true,
            occurred_at: None,
            data: None,
        }],
        next_cursor: 3,
        has_more: false,
        gap: false,
    };
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["nextCursor"], 3);
    assert_eq!(v["items"][0]["type"], T);
    assert!(v["items"][0].get("data").is_none());
    assert_eq!(parse_page(&serde_json::to_vec(&p).unwrap(), 0).unwrap(), p);
}
