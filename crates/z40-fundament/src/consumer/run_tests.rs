use super::*;
use crate::consumer::CursorState;
use crate::health::RunResult;
use crate::protocol::feed::FeedEvent;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const K: &str = "0192f39e-0000-7000-8000-000000000001";

type Answer = Arc<dyn Fn(u64) -> (StatusCode, String) + Send + Sync>;

/// Fake feed: answers per `since`, records the requests.
#[derive(Clone)]
struct Feed {
    answer: Answer,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn events(
    State(f): State<Feed>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, String) {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    f.seen.lock().unwrap().push(format!(
        "{}|{}|{}|{}",
        q["since"],
        q["limit"],
        q.get("types").map_or("-", String::as_str),
        auth.unwrap_or("-")
    ));
    (f.answer)(q["since"].parse().unwrap())
}

async fn serve(answer: Answer) -> (String, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(crate::protocol::EVENTS_PATH, get(events))
        .with_state(Feed {
            answer,
            seen: Arc::clone(&seen),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn page(cursor: u64, more: bool, gap: bool) -> (StatusCode, String) {
    let items = if cursor == 0 {
        json!([])
    } else {
        json!([{ "cursor": cursor, "type": "urn:example:crm:customer", "key": K,
                 "version": cursor, "data": { "name": format!("v{cursor}") } }])
    };
    let body = json!({ "items": items, "nextCursor": cursor, "hasMore": more, "gap": gap });
    (StatusCode::OK, body.to_string())
}

/// In-memory store recording what happens.
#[derive(Default)]
struct Memory {
    locked: Mutex<bool>,
    state: Mutex<Option<CursorState>>,
    applied: Mutex<Vec<(u64, usize)>>,
    resets: Mutex<usize>,
    finished: Mutex<Vec<Option<FetchError>>>,
    fail_apply: bool,
}

impl RunLock for Memory {
    type Guard = ();
    async fn try_lock(&self, _: &str) -> Result<Option<()>, StoreError> {
        let mut l = self.locked.lock().unwrap();
        Ok((!*l).then(|| *l = true))
    }
    async fn unlock(&self, _: ()) -> Result<(), StoreError> {
        *self.locked.lock().unwrap() = false;
        Ok(())
    }
}

impl CursorStore for Memory {
    async fn load(&self, _: &str) -> Result<Option<CursorState>, StoreError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn reset(&self, _: &str) -> Result<(), StoreError> {
        *self.resets.lock().unwrap() += 1;
        self.state.lock().unwrap().get_or_insert_default().cursor = 0;
        Ok(())
    }
    async fn finish_run(
        &self,
        _: &str,
        e: Option<FetchError>,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.finished.lock().unwrap().push(e);
        let mut s = self.state.lock().unwrap();
        let h = &mut s.get_or_insert_default().health;
        h.last_run = Some(at);
        h.last_result = Some(if e.is_none() {
            RunResult::Ok
        } else {
            RunResult::Failed
        });
        Ok(())
    }
}

impl PageSink for Memory {
    async fn apply_page(&self, _: &str, ev: &[FeedEvent], next: u64) -> Result<usize, StoreError> {
        if self.fail_apply {
            return Err(StoreError::new(std::io::Error::other("disk full")));
        }
        self.applied.lock().unwrap().push((next, ev.len()));
        self.state.lock().unwrap().get_or_insert_default().cursor = next;
        Ok(ev.len())
    }
}

fn consumer(base: &str) -> FeedConsumerBuilder {
    FeedConsumerBuilder::plain_http_for_tests("crm", base, BearerKey::new("k3y").unwrap()).unwrap()
}

#[tokio::test]
async fn pages_until_no_more_and_sends_key_and_parameters() {
    let (base, seen) = serve(Arc::new(|since| page(since + 1, since < 2, false))).await;
    let c = consumer(&base)
        .types(["urn:a", "urn:b"])
        .page_limit(10)
        .build()
        .unwrap();
    let m = Memory::default();
    let r = c.run_once(&m).await.unwrap();
    assert_eq!(
        r,
        RunReport {
            outcome: RunOutcome::Ok,
            pages: 3,
            written: 3,
            cursor: 3
        }
    );
    assert_eq!(*m.applied.lock().unwrap(), [(1, 1), (2, 1), (3, 1)]);
    assert_eq!(seen.lock().unwrap()[0], "0|10|urn:a,urn:b|Bearer k3y");
    assert_eq!(*m.finished.lock().unwrap(), [None]);
    assert!(!*m.locked.lock().unwrap(), "lock released");
}

#[tokio::test]
async fn page_budget_and_continuation() {
    let (base, seen) = serve(Arc::new(|since| page(since + 1, true, false))).await;
    let c = consumer(&base).max_pages(2).build().unwrap();
    let m = Memory::default();
    assert_eq!(c.run_once(&m).await.unwrap().cursor, 2);
    assert_eq!(
        c.run_once(&m).await.unwrap().cursor,
        4,
        "next run continues at the cursor"
    );
    assert_eq!(
        seen.lock().unwrap()[0],
        "0|500|-|Bearer k3y",
        "no types = all types"
    );
}

#[tokio::test]
async fn gap_resets_to_zero_and_reads_again() {
    let (base, seen) = serve(Arc::new(|since| match since {
        7 => page(0, true, true),
        _ => page(since + 1, false, false),
    }))
    .await;
    let m = Memory::default();
    *m.state.lock().unwrap() = Some(CursorState {
        cursor: 7,
        ..Default::default()
    });
    let r = consumer(&base).build().unwrap().run_once(&m).await.unwrap();
    assert_eq!((r.outcome, r.pages, r.cursor), (RunOutcome::Ok, 2, 1));
    assert_eq!(*m.resets.lock().unwrap(), 1);
    let seen = seen.lock().unwrap();
    assert!(seen[0].starts_with("7|") && seen[1].starts_with("0|"));
}

#[tokio::test]
async fn source_failures_are_recorded_and_keep_progress() {
    let (base, _) = serve(Arc::new(|since| match since {
        0 => page(1, true, false),
        1 => (StatusCode::SERVICE_UNAVAILABLE, "down".into()),
        _ => (StatusCode::OK, "<html>".into()),
    }))
    .await;
    let m = Memory::default();
    let r = consumer(&base).build().unwrap().run_once(&m).await.unwrap();
    assert_eq!(r.outcome, RunOutcome::Failed(FetchError::Status(503)));
    assert_eq!((r.pages, r.cursor), (2, 1), "first page stays applied");
    assert_eq!(*m.finished.lock().unwrap(), [Some(FetchError::Status(503))]);
    let (bad, _) = serve(Arc::new(|_| (StatusCode::OK, "<html>".into()))).await;
    let r = consumer(&bad).build().unwrap().run_once(&m).await.unwrap();
    assert_eq!(r.outcome, RunOutcome::Failed(FetchError::Invalid));
    let gone = consumer("http://127.0.0.1:1").build().unwrap();
    let r = gone.run_once(&Memory::default()).await.unwrap();
    assert_eq!(r.outcome, RunOutcome::Failed(FetchError::Unreachable));
}

#[tokio::test]
async fn locked_and_store_failures() {
    let (base, seen) = serve(Arc::new(|since| page(since + 1, false, false))).await;
    let c = consumer(&base).build().unwrap();
    let m = Memory::default();
    *m.locked.lock().unwrap() = true;
    assert_eq!(c.run_once(&m).await.unwrap().outcome, RunOutcome::Locked);
    assert!(seen.lock().unwrap().is_empty(), "no request while locked");
    let failing = Memory {
        fail_apply: true,
        ..Default::default()
    };
    assert!(c.run_once(&failing).await.is_err());
    assert!(
        !*failing.locked.lock().unwrap(),
        "lock released after a store error"
    );
    assert!(failing.finished.lock().unwrap().is_empty());
}

#[test]
fn builder_rules() {
    let key = || BearerKey::new("k").unwrap();
    assert!(FeedConsumer::builder("crm", "https://crm.example.com", key()).is_ok());
    assert_eq!(
        FeedConsumer::builder("crm", "http://crm.example.com", key()).unwrap_err(),
        BuildError::InvalidUrl
    );
    assert_eq!(
        FeedConsumerBuilder::plain_http_for_tests("crm", "http://crm.example.com", key())
            .unwrap_err(),
        BuildError::InvalidUrl,
        "plain http only on loopback"
    );
    assert_eq!(
        FeedConsumer::builder("c r m", "https://crm.example.com", key()).unwrap_err(),
        BuildError::InvalidSource
    );
    let b = FeedConsumer::builder("crm", "https://crm.example.com", key()).unwrap();
    assert_eq!(
        b.clone().page_limit(0).build().unwrap_err(),
        BuildError::InvalidPageLimit
    );
    assert_eq!(
        b.clone().page_limit(501).build().unwrap_err(),
        BuildError::InvalidPageLimit
    );
    assert_eq!(
        b.max_pages(0).build().unwrap_err(),
        BuildError::InvalidMaxPages
    );
    let off = FeedConsumerConfig {
        url: None,
        key: Some(key()),
        interval_secs: 60,
    };
    assert!(FeedConsumer::from_config("crm", &off).unwrap().is_none());
    let on = FeedConsumerConfig {
        url: Some("https://crm.example.com".into()),
        ..off
    };
    let c = FeedConsumer::from_config("crm", &on)
        .unwrap()
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(c.source(), "crm");
}
