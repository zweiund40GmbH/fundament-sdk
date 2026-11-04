use super::*;
use serde_json::json;
use std::sync::Mutex;

const K: &str = "0192f39e-0000-7000-8000-00000000000A";
const T: &str = "urn:example:crm:customer";

fn event(deleted: bool, data: Option<Value>) -> FeedEvent {
    FeedEvent {
        cursor: 1,
        id: None,
        event_type: T.into(),
        key: K.into(),
        version: 4,
        deleted,
        occurred_at: None,
        data,
    }
}

#[test]
fn keep_fields_keeps_only_named_fields() {
    let m = KeepFields::new().keep(T, "customer", &["name", "address.city", "address.zip", "n"]);
    let data = json!({
        "name": "  Example Ltd ", "phone": "0123", "email": "a@example.com", "note": "private",
        "address": { "street": "Main St 1", "city": "Springfield", "zip": "" }, "n": 3
    });
    let p = m.project("customer", &data);
    assert_eq!(
        p.data,
        Some(json!({ "name": "Example Ltd", "address": { "city": "Springfield" }, "n": 3 }))
    );
    let text = p.data.unwrap().to_string();
    assert!(!text.contains("0123") && !text.contains("a@example.com") && !text.contains("private"));
    assert_eq!(m.kind(T), Some("customer"));
    assert_eq!(m.kind("urn:example:crm:contact"), None);
    assert_eq!(m.project("customer", &json!({})).data, None);
    let long = m.project("customer", &json!({ "name": "x".repeat(500) }));
    assert_eq!(long.data.unwrap()["name"].as_str().unwrap().len(), TEXT_MAX);
}

#[test]
fn text_helper() {
    let v = json!({ "a": " x ", "b": "  ", "c": 1 });
    assert_eq!(text_field(&v, "a").as_deref(), Some("x"));
    assert_eq!(text_field(&v, "b"), None);
    assert_eq!(text_field(&v, "c"), None);
    assert_eq!(text_field(&v, "d"), None);
}

/// A mapping with an external id and a default projection check.
struct WithRef;

impl Mapping for WithRef {
    fn kind(&self, event_type: &str) -> Option<&str> {
        (event_type == T).then_some("customer")
    }
    fn project(&self, _: &str, data: &Value) -> Projection {
        Projection {
            data: None,
            external_id: text_field(data, "ref"),
        }
    }
}

/// Keeps no fields: the default projection.
struct Nothing;

impl Mapping for Nothing {
    fn kind(&self, _: &str) -> Option<&str> {
        Some("any")
    }
}

#[test]
fn records_and_tombstones() {
    let r = record(
        &WithRef,
        &event(false, Some(json!({ "ref": "17", "name": "A" }))),
    )
    .unwrap();
    assert_eq!(r.key, K.to_ascii_lowercase());
    assert_eq!(
        (r.kind.as_str(), r.version, r.deleted),
        ("customer", 4, false)
    );
    assert_eq!(r.external_id.as_deref(), Some("17"));
    let t = record(&WithRef, &event(true, Some(json!({ "ref": "17" })))).unwrap();
    assert_eq!((t.deleted, t.external_id, t.data), (true, None, None));
    let too_long = record(
        &WithRef,
        &event(false, Some(json!({ "ref": "9".repeat(65) }))),
    )
    .unwrap();
    assert_eq!(too_long.external_id, None);
    let nothing = record(&Nothing, &event(false, Some(json!({ "name": "A" })))).unwrap();
    assert_eq!(nothing.data, None, "default keeps no data");
    let mut other = event(false, Some(json!({})));
    other.event_type = "urn:example:crm:contact".into();
    assert!(record(&WithRef, &other).is_none());
}

#[derive(Default)]
struct Store(Mutex<Vec<(usize, u64)>>);

impl ReplicaStore for Store {
    async fn apply_records(&self, _: &str, r: &[Record], next: u64) -> Result<usize, StoreError> {
        self.0.lock().unwrap().push((r.len(), next));
        Ok(r.len())
    }
}

#[tokio::test]
async fn adapter_maps_pages() {
    let replica = Replica::new(Store::default(), WithRef);
    let mut skipped = event(false, Some(json!({})));
    skipped.event_type = "urn:example:crm:contact".into();
    let n = replica
        .apply_page("crm", &[event(false, Some(json!({}))), skipped], 9)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(*replica.store().0.lock().unwrap(), [(1, 9)]);
}
