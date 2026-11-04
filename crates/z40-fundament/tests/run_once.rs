//! A full run: fake feed server on 127.0.0.1, real MariaDB (`TEST_DATABASE_URL`), `KeepFields`
//! mapping, and the status document built from what the run stored.
#![cfg(all(feature = "replica-mysql", feature = "status"))]

mod common;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::{feed_server, with_db};
use serde_json::{Value, json};
use std::sync::Arc;
use z40_fundament::consumer::{CursorStore, FeedConsumerBuilder, FetchError, RunOutcome};
use z40_fundament::credentials::BearerKey;
use z40_fundament::protocol::Level;
use z40_fundament::replica::{KeepFields, Replica};
use z40_fundament::replica_mysql::{MySqlStore, Tables};
use z40_fundament::status::StatusBuilder;

const T: &str = "urn:example:crm:customer";

fn key(n: u64) -> String {
    format!("0192f39e-0000-7000-8000-{n:012}")
}

fn event(cursor: u64, n: u64, version: u64, deleted: bool) -> Value {
    let data = json!({ "name": format!("Customer {n}"), "email": "person@example.com",
                       "address": { "city": "Springfield", "street": "Main St 1" } });
    json!({ "cursor": cursor, "type": T, "key": key(n), "version": version, "deleted": deleted,
            "data": if deleted { Value::Null } else { data } })
}

fn page(items: Vec<Value>, next: u64, more: bool, gap: bool) -> (StatusCode, String) {
    let body = json!({ "items": items, "nextCursor": next, "hasMore": more, "gap": gap });
    (StatusCode::OK, body.to_string())
}

/// The feed: two pages from 0, a gap for the stale cursor 99, and a failure for cursor 4.
fn answer(since: u64) -> (StatusCode, String) {
    match since {
        0 => page(
            vec![event(1, 1, 1, false), event(2, 2, 1, false)],
            2,
            true,
            false,
        ),
        2 => page(
            vec![event(3, 1, 2, true), event(4, 3, 1, false)],
            4,
            false,
            false,
        ),
        99 => page(vec![], 0, true, true),
        _ => (StatusCode::BAD_GATEWAY, "upstream down".into()),
    }
}

#[tokio::test]
async fn full_run_into_mariadb() {
    let tables = Tables::with_prefix("app_").unwrap();
    with_db("run", tables.clone(), |pool| async move {
        let base = feed_server(Arc::new(answer)).await;
        let consumer =
            FeedConsumerBuilder::plain_http_for_tests("crm", &base, BearerKey::new("k").unwrap())
                .unwrap()
                .types([T])
                .build()
                .unwrap();
        let mapping = KeepFields::new()
            .keep(T, "customer", &["name", "address.city"])
            .unwrap();
        let store = Replica::new(MySqlStore::new(pool.clone(), tables.clone()), mapping);

        let r = consumer.run_once(&store).await.unwrap();
        assert_eq!(
            (r.outcome, r.pages, r.written, r.cursor),
            (RunOutcome::Ok, 2, 4, 4)
        );
        let db = store.store();
        let live = db.records("crm", "customer").await.unwrap();
        assert_eq!(
            live.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
            [key(2), key(3)]
        );
        assert_eq!(
            live[0].data,
            Some(json!({ "name": "Customer 2", "address": { "city": "Springfield" } })),
            "only the kept fields, no e-mail address or street"
        );
        assert!(
            db.record("crm", "customer", &key(1))
                .await
                .unwrap()
                .unwrap()
                .deleted
        );

        // Next run: the source fails; the cursor stays and the failure is recorded.
        let r = consumer.run_once(&store).await.unwrap();
        assert_eq!(r.outcome, RunOutcome::Failed(FetchError::Status(502)));
        let state = store.load("crm").await.unwrap().unwrap();
        assert_eq!(state.cursor, 4);
        assert_eq!(state.health.text.as_deref(), Some("HTTP 502"));

        // A stale cursor gets a gap: read again from 0, versions keep the newer states.
        sqlx::query("UPDATE app_fundament_cursor SET `cursor` = 99")
            .execute(&pool)
            .await
            .unwrap();
        let r = consumer.run_once(&store).await.unwrap();
        assert_eq!((r.outcome, r.written, r.cursor), (RunOutcome::Ok, 0, 4));
        assert!(
            db.record("crm", "customer", &key(1))
                .await
                .unwrap()
                .unwrap()
                .deleted
        );

        // The status document from the stored health and events.
        let now = Utc::now();
        let state = store.load("crm").await.unwrap().unwrap();
        let mut b = StatusBuilder::new("billing", "1.0.0", now).feed_source(
            "crm",
            &state.health,
            Duration::minutes(15),
        );
        for e in db
            .recent_events(now - Duration::days(30), 100)
            .await
            .unwrap()
        {
            b = b.event(e.time, e.kind, e.level, e.text);
        }
        let doc = b.build();
        assert_eq!(doc.state, Level::Ok);
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["bezieht"][0]["quelle"], "crm");
        assert_eq!(v["ereignisse"][0]["text"], "Feed von crm wieder erreichbar");
        assert_eq!(v["ereignisse"][1]["text"], "Feed von crm: HTTP 502");
    })
    .await;
}

/// A run cancelled while it waits for the source must not leave the lock behind: the next run on the
/// same pool runs (`Ok`), it is not `Locked`.
#[tokio::test]
async fn cancelled_run_releases_the_lock() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let tables = Tables::with_prefix("c_").unwrap();
    with_db("cancel", tables.clone(), |pool| async move {
        // The source does not answer within the first run's time.
        let delay = Arc::new(AtomicU64::new(10_000));
        let answer: common::Answer = Arc::new(|_| page(vec![], 0, false, false));
        let base = common::feed_server_delayed(answer, Arc::clone(&delay)).await;
        let consumer =
            FeedConsumerBuilder::plain_http_for_tests("crm", &base, BearerKey::new("k").unwrap())
                .unwrap()
                .build()
                .unwrap();
        let store = Replica::new(MySqlStore::new(pool.clone(), tables), KeepFields::new());
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            consumer.run_once(&store),
        )
        .await;
        assert!(cancelled.is_err(), "the first run was cancelled");
        delay.store(0, Ordering::SeqCst);
        // The connection of the cancelled run closes in the background; give the server a moment.
        // A pooled session that kept the lock would let the next run in (locks nest per session), so
        // check that the lock is really free.
        let lock_name = "LEFT(CONCAT('fundament_feed:', 'crm', ':', DATABASE()), 64)";
        let mut free = false;
        for _ in 0..50 {
            let f: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT IS_FREE_LOCK({lock_name})"
            )))
            .fetch_one(&pool)
            .await
            .unwrap();
            free = f == Some(1);
            if free {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(free, "the lock of the cancelled run is released");
        let mut outcome = RunOutcome::Locked;
        for _ in 0..50 {
            outcome = consumer.run_once(&store).await.unwrap().outcome;
            if outcome != RunOutcome::Locked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(outcome, RunOutcome::Ok);
    })
    .await;
}
