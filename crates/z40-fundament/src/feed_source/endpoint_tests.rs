use super::*;
use crate::StoreError;
use crate::credentials::{MAX_FAILURES, key_entry};
use crate::feed_source::AssignOutcome;
use crate::protocol::feed::FeedEvent;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request as HttpRequest, header};
use chrono::DateTime;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;
use tower::ServiceExt;

const T: &str = "urn:example:crm:customer";

/// An in-memory store that logs the calls in order.
struct Fake {
    types: Vec<String>,
    events: Vec<FeedEvent>,
    fail_read: bool,
    log: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn push(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }
}

#[derive(Debug, thiserror::Error)]
#[error("database down")]
struct Down;

fn ev(cursor: u64) -> FeedEvent {
    FeedEvent {
        cursor,
        id: Some(cursor.to_string()),
        event_type: T.into(),
        key: "0192f3a1-7c2e-7000-8000-00000000002a".into(),
        version: cursor,
        deleted: false,
        occurred_at: Some("2026-11-04T09:00:00Z".into()),
        data: Some(json!({ "name": "A" })),
    }
}

impl FeedSourceStore for Fake {
    fn known_types(&self) -> &[String] {
        &self.types
    }
    async fn assign_cursors(&self, max_wait: Duration) -> Result<AssignOutcome, StoreError> {
        assert_eq!(
            max_wait,
            crate::feed_source::REQUEST_ASSIGN_WAIT,
            "short wait"
        );
        self.push("assign".into());
        Ok(AssignOutcome::Assigned(0))
    }
    async fn read(
        &self,
        since: u64,
        limit: u32,
        types: &[String],
    ) -> Result<Vec<FeedEvent>, StoreError> {
        self.push(format!("read {since} {limit} {}", types.join("|")));
        if self.fail_read {
            return Err(StoreError::new(Down));
        }
        Ok(self
            .events
            .iter()
            .filter(|e| e.cursor > since)
            .take(limit as usize)
            .cloned()
            .collect())
    }
    async fn record_poll(
        &self,
        consumer: &str,
        next: u64,
        _at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.push(format!("poll {consumer} {next}"));
        Ok(())
    }
    async fn record_failures(&self, n: u64, _at: DateTime<Utc>) -> Result<(), StoreError> {
        for _ in 0..n {
            self.push("failure".into());
        }
        Ok(())
    }
}

fn setup(fail_read: bool) -> (Router, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let store = Fake {
        types: vec![T.into(), "urn:example:crm:site".into()],
        events: (1..=5).map(ev).collect(),
        fail_read,
        log: Arc::clone(&log),
    };
    let keys = KeyList::parse(&key_entry("crm", "good-key").unwrap()).unwrap();
    (FeedEndpoint::new(store, keys).router(), log)
}

async fn call(r: &Router, auth: Option<&str>, query: &str, ip: &str) -> (StatusCode, Response) {
    let mut req = HttpRequest::get(format!("{EVENTS_PATH}{query}"));
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    let mut req = req.body(Body::empty()).unwrap();
    let addr: SocketAddr = format!("{ip}:1234").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let res = r.clone().oneshot(req).await.unwrap();
    (res.status(), res)
}

async fn body(res: Response) -> Value {
    serde_json::from_slice(&to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap()
}

#[tokio::test]
async fn page_with_has_more_and_poll_record() {
    let (r, log) = setup(false);
    let (s, res) = call(
        &r,
        Some("Bearer good-key"),
        "?since=1&limit=2",
        "203.0.113.1",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let v = body(res).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        (v["nextCursor"].clone(), v["hasMore"].clone()),
        (json!(3), json!(true))
    );
    assert_eq!(v["gap"], false);
    let log = log.lock().unwrap().clone();
    let types = format!("{T}|urn:example:crm:site");
    assert_eq!(
        log,
        [
            "assign".to_owned(),
            format!("read 1 3 {types}"),
            "poll crm 3".to_owned()
        ],
        "assign before read, limit + 1, poll remembered with the key name"
    );
}

#[tokio::test]
async fn check_order_brake_key_query() {
    let (r, log) = setup(false);
    let (s, res) = call(&r, None, "?limit=0", "203.0.113.2").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "key before query");
    assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(body(res).await["code"], "UNAUTHENTICATED");
    for _ in 1..MAX_FAILURES {
        call(&r, Some("Bearer wrong"), "", "203.0.113.2").await;
    }
    let (s, res) = call(&r, Some("Bearer good-key"), "", "203.0.113.2").await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "brake before key");
    assert_eq!(res.headers()[header::RETRY_AFTER], "60");
    assert_eq!(body(res).await["code"], "RATE_LIMITED");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let failures = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| *l == "failure")
        .count();
    assert_eq!(
        failures, MAX_FAILURES as usize,
        "each 401 counted, the 429 not"
    );
    assert!(
        !log.lock().unwrap().iter().any(|l| l == "assign"),
        "nothing is read without a key"
    );
}

#[tokio::test]
async fn invalid_queries_are_400() {
    let (r, log) = setup(false);
    for q in [
        "?limit=0",
        "?limit=501",
        "?since=-1",
        "?types=urn:example:x:y",
        "?since=1&since=2",
    ] {
        let (s, res) = call(&r, Some("Bearer good-key"), q, "203.0.113.3").await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
        let v = body(res).await;
        assert_eq!(v["code"], "BAD_REQUEST");
        assert!(v["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    assert!(
        log.lock().unwrap().is_empty(),
        "no assignment for invalid queries"
    );
    let (s, _) = call(
        &r,
        Some("Bearer good-key"),
        &format!("?types={T}"),
        "203.0.113.3",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        log.lock().unwrap()[1],
        format!("read 0 101 {T}"),
        "default limit 100"
    );
}

#[tokio::test]
async fn store_failure_is_500_without_details() {
    let (r, _) = setup(true);
    let (s, res) = call(&r, Some("Bearer good-key"), "", "203.0.113.4").await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    let v = body(res).await;
    assert_eq!(v["code"], "INTERNAL");
    assert!(!v.to_string().contains("database"));
}

#[tokio::test]
async fn failed_attempt_hook_fires_on_401_not_on_429() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let count = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&count);
    let store = Fake {
        types: vec![T.into()],
        events: Vec::new(),
        fail_read: false,
        log: Arc::default(),
    };
    let keys = KeyList::parse(&key_entry("crm", "good-key").unwrap()).unwrap();
    let r: Router = FeedEndpoint::new(store, keys)
        .on_failed_attempt(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .router();
    for _ in 0..(MAX_FAILURES + 3) {
        call(&r, Some("Bearer wrong"), "", "203.0.113.9").await;
    }
    let (s, _) = call(&r, Some("Bearer good-key"), "", "203.0.113.9").await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        count.load(Ordering::SeqCst),
        u64::from(MAX_FAILURES),
        "braked ones are not counted"
    );
    let (s, _) = call(&r, Some("Bearer good-key"), "", "203.0.113.10").await;
    assert_eq!(s, StatusCode::OK, "a valid key does not fire the hook");
    assert_eq!(count.load(Ordering::SeqCst), u64::from(MAX_FAILURES));
}
