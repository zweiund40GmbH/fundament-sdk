//! Integration tests of `MySqlStore` against a real MariaDB (`TEST_DATABASE_URL`).
#![cfg(feature = "replica-mysql")]

mod common;

use chrono::{Duration, Utc};
use common::with_db;
use serde_json::json;
use z40_fundament::consumer::{CursorStore, FetchError, RunLock};
use z40_fundament::health::RunResult;
use z40_fundament::protocol::Level;
use z40_fundament::replica::{Record, ReplicaStore};
use z40_fundament::replica_mysql::{MySqlStore, Tables};

const K: &str = "0192f39e-0000-7000-8000-000000000001";

fn rec(version: u64, deleted: bool, name: &str) -> Record {
    Record {
        kind: "customer".into(),
        key: K.into(),
        version,
        deleted,
        external_id: (!deleted).then(|| "17".into()),
        data: (!deleted).then(|| json!({ "name": name })),
    }
}

#[tokio::test]
async fn version_rule_tombstones_and_cursor() {
    let tables = Tables::with_prefix("t_").unwrap();
    with_db("apply", tables.clone(), |pool| async move {
        let s = MySqlStore::new(pool, tables);
        assert_eq!(
            s.apply_records("crm", &[rec(2, false, "B")], 5)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            s.load("crm").await.unwrap().unwrap().cursor,
            5,
            "cursor with the page"
        );
        // Older and equal versions are dropped; applying twice changes nothing.
        let n = s
            .apply_records("crm", &[rec(1, false, "A"), rec(2, false, "X")], 6)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let r = s
            .record("crm", "customer", &K.to_ascii_uppercase())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((r.version, r.data), (2, Some(json!({ "name": "B" }))));
        assert_eq!(r.external_id.as_deref(), Some("17"));
        assert_eq!(s.records("crm", "customer").await.unwrap().len(), 1);
        // A tombstone clears the data and keeps its version.
        assert_eq!(
            s.apply_records("crm", &[rec(3, true, "")], 7)
                .await
                .unwrap(),
            1
        );
        let t = s.record("crm", "customer", K).await.unwrap().unwrap();
        assert!(t.deleted && t.data.is_none() && t.external_id.is_none());
        assert_eq!(t.version, 3);
        assert!(s.records("crm", "customer").await.unwrap().is_empty());
        assert!(
            s.record("billing", "customer", K).await.unwrap().is_none(),
            "per source"
        );
    })
    .await;
}

#[tokio::test]
async fn run_results_events_and_reset() {
    let tables = Tables::new("rep", "cur", "evt").unwrap();
    with_db("cursor", tables.clone(), |pool| async move {
        let s = MySqlStore::new(pool.clone(), tables).event_kind("bezug");
        assert!(s.load("crm").await.unwrap().is_none());
        let t0 = Utc::now() - Duration::minutes(10);
        s.finish_run("crm", Some(FetchError::Status(503)), t0)
            .await
            .unwrap();
        s.finish_run("crm", Some(FetchError::Unreachable), t0)
            .await
            .unwrap();
        let h = s.load("crm").await.unwrap().unwrap().health;
        assert_eq!(h.last_result, Some(RunResult::Failed));
        assert_eq!(h.text.as_deref(), Some("nicht erreichbar"));
        assert!(h.last_success.is_none());
        let t1 = t0 + Duration::minutes(1);
        s.finish_run("crm", None, t1).await.unwrap();
        let h = s.load("crm").await.unwrap().unwrap().health;
        assert_eq!((h.last_result, h.text), (Some(RunResult::Ok), None));
        assert_eq!(h.last_success.map(|t| t.timestamp()), Some(t1.timestamp()));
        let ev = s
            .recent_events(t0 - Duration::minutes(1), 100)
            .await
            .unwrap();
        let texts: Vec<_> = ev
            .iter()
            .map(|e| (e.level, e.text.as_str(), e.kind.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                (Level::Ok, "Feed von crm wieder erreichbar", "bezug"),
                (Level::Error, "Feed von crm: HTTP 503", "bezug"),
            ],
            "one event per change, newest first"
        );
        // Old events are purged at the end of a run.
        let old = (Utc::now() - Duration::days(31)).naive_utc();
        sqlx::query(
            "INSERT INTO evt (`time`, `kind`, `level`, `text`) VALUES (?, 'x', 'ok', 'old')",
        )
        .bind(old)
        .execute(&pool)
        .await
        .unwrap();
        s.finish_run("crm", None, Utc::now()).await.unwrap();
        let all = s
            .recent_events(Utc::now() - Duration::days(60), 100)
            .await
            .unwrap();
        assert!(all.iter().all(|e| e.text != "old"));
        s.apply_records("crm", &[], 9).await.unwrap();
        s.reset("crm").await.unwrap();
        assert_eq!(s.load("crm").await.unwrap().unwrap().cursor, 0);
    })
    .await;
}

#[tokio::test]
async fn one_run_per_source() {
    let tables = Tables::with_prefix("").unwrap();
    with_db("lock", tables.clone(), |pool| async move {
        let s = MySqlStore::new(pool, tables);
        let g = s.try_lock("crm").await.unwrap().expect("first lock");
        assert!(
            s.try_lock("crm").await.unwrap().is_none(),
            "held by the first run"
        );
        let other = s.try_lock("billing").await.unwrap().expect("other source");
        s.unlock(g).await.unwrap();
        let again = s.try_lock("crm").await.unwrap().expect("released");
        s.unlock(again).await.unwrap();
        s.unlock(other).await.unwrap();
    })
    .await;
}
