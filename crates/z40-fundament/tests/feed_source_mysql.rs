//! Integration tests of `MySqlSource` against a real MariaDB (`TEST_DATABASE_URL`): outbox in the app's
//! transaction, cursor assignment after commit, retention and the poll log.
#![cfg(feature = "feed-source-mysql")]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

mod common;

use chrono::{Duration, Utc};
use common::with_schema;
use serde_json::json;
use sqlx::MySqlPool;
use std::time::Instant;
use z40_fundament::feed_source::{AssignOutcome, FeedSourceStore, SourceStatusStore};
use z40_fundament::feed_source_mysql::{MySqlSource, OutboxEvent, SourceTables};

const T: &str = "urn:example:crm:customer";

fn key(n: u64) -> String {
    format!("0192f39e-0000-7000-8000-{n:012}")
}

fn source(pool: MySqlPool, tables: SourceTables) -> MySqlSource {
    MySqlSource::new(pool, tables, [T]).unwrap()
}

/// Writes `e` in its own committed transaction.
async fn commit(s: &MySqlSource, e: OutboxEvent) {
    let mut tx = s.pool().begin().await.unwrap();
    s.write_event(&mut tx, &e).await.unwrap();
    tx.commit().await.unwrap();
}

fn state(n: u64, version: u64) -> OutboxEvent {
    OutboxEvent::state(
        T,
        key(n),
        version,
        json!({ "name": format!("Customer {n}") }),
    )
}

/// (key, cursor) of every readable event since 0.
async fn feed(s: &MySqlSource) -> Vec<(String, u64)> {
    let all = s.read(0, 500, &[T.to_owned()]).await.unwrap();
    all.into_iter().map(|e| (e.key, e.cursor)).collect()
}

#[tokio::test]
async fn outbox_lives_in_the_app_transaction() {
    let tables = SourceTables::with_prefix("o_").unwrap();
    with_schema("src_outbox", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        let mut tx = s.pool().begin().await.unwrap();
        s.write_event(&mut tx, &state(1, 1)).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(
            s.assign_cursors().await.unwrap(),
            AssignOutcome::Assigned(0)
        );
        assert!(
            feed(&s).await.is_empty(),
            "a rolled back change leaves no event"
        );

        commit(&s, state(1, 1)).await;
        commit(&s, OutboxEvent::tombstone(T, key(2).to_uppercase(), 3)).await;
        assert!(
            feed(&s).await.is_empty(),
            "no cursor before the assigner ran"
        );
        assert_eq!(
            s.assign_cursors().await.unwrap(),
            AssignOutcome::Assigned(2)
        );
        let events = s.read(0, 10, &[T.to_owned()]).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].cursor, events[1].cursor), (1, 2));
        assert_eq!(events[0].data, Some(json!({ "name": "Customer 1" })));
        assert!(events[1].deleted && events[1].data.is_none());
        assert_eq!(events[1].key, key(2), "keys in lower case");
        assert_eq!(s.last_cursor().await.unwrap(), Some(2));
        assert!(
            s.read(1, 10, &["urn:example:crm:site".to_owned()])
                .await
                .unwrap()
                .is_empty()
        );

        let mut tx = s.pool().begin().await.unwrap();
        assert!(
            s.write_event(&mut tx, &state(1, 1)).await.is_err(),
            "a version is written once"
        );
    })
    .await;
}

#[tokio::test]
async fn open_transaction_is_waited_for_and_never_skipped() {
    let tables = SourceTables::with_prefix("").unwrap();
    with_schema("src_assign", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        commit(&s, state(1, 1)).await;
        assert_eq!(
            s.assign_cursors().await.unwrap(),
            AssignOutcome::Assigned(1)
        );

        // A second connection holds an uncommitted event; a later event commits meanwhile.
        let mut open = s.pool().begin().await.unwrap();
        s.write_event(&mut open, &state(2, 1)).await.unwrap();
        commit(&s, state(3, 1)).await;

        let started = Instant::now();
        let (first, second) = tokio::join!(s.assign_cursors(), async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            s.assign_cursors().await
        });
        assert_eq!(
            first.unwrap(),
            AssignOutcome::Waiting(0),
            "waits, never skips"
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(4),
            "bounded wait"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        assert_eq!(
            second.unwrap(),
            AssignOutcome::Skipped,
            "one assigner at a time"
        );
        assert_eq!(feed(&s).await, [(key(1), 1)], "the later event waits too");

        open.commit().await.unwrap();
        assert_eq!(
            s.assign_cursors().await.unwrap(),
            AssignOutcome::Assigned(2)
        );
        assert_eq!(
            feed(&s).await,
            [(key(1), 1), (key(2), 2), (key(3), 3)],
            "after commit it gets a higher cursor than everything assigned before"
        );
        let after_first = s.read(1, 10, &[T.to_owned()]).await.unwrap();
        assert_eq!(after_first.len(), 2, "a consumer at cursor 1 sees both");
    })
    .await;
}

#[tokio::test]
async fn retention_keeps_the_newest_state_of_every_record() {
    let tables = SourceTables::with_prefix("r_").unwrap();
    with_schema("src_retention", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        let old = Utc::now() - Duration::days(40);
        commit(&s, state(1, 1).at(old)).await;
        commit(&s, state(1, 2).at(old)).await;
        commit(&s, OutboxEvent::tombstone(T, key(1), 3).at(old)).await;
        commit(&s, state(2, 1).at(old)).await;
        commit(&s, state(3, 1)).await;
        commit(&s, state(3, 2)).await;
        s.assign_cursors().await.unwrap();
        // An old superseded event without cursor (never assigned) is not touched either.
        commit(&s, state(4, 1).at(old)).await;
        commit(&s, state(4, 2).at(old)).await;

        let gone = s
            .purge(std::time::Duration::from_secs(30 * 86_400))
            .await
            .unwrap();
        assert_eq!(gone, 2, "only the superseded old states of record 1");
        let kept: Vec<_> = s
            .read(0, 100, &[T.to_owned()])
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.key, e.version, e.deleted))
            .collect();
        assert_eq!(
            kept,
            [
                (key(1), 3, true),
                (key(2), 1, false),
                (key(3), 1, false),
                (key(3), 2, false)
            ],
            "tombstone and newest states stay; recent superseded ones too"
        );
        s.assign_cursors().await.unwrap();
        assert_eq!(feed(&s).await.len(), 6);
    })
    .await;
}

#[tokio::test]
async fn poll_log_failures_and_status_figures() {
    let tables = SourceTables::new("ob", "cnt", "pl", "fl").unwrap();
    with_schema("src_status", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        for n in 1..=3 {
            commit(&s, state(n, 1)).await;
        }
        commit(&s, state(9, 1).at(Utc::now() - Duration::hours(2))).await;
        s.assign_cursors().await.unwrap();
        let now = Utc::now();
        s.record_poll("crm", 1, now - Duration::minutes(1))
            .await
            .unwrap();
        s.record_poll("crm", 2, now).await.unwrap();
        let polls = s.polls().await.unwrap();
        assert_eq!(polls.len(), 1);
        assert_eq!((polls[0].consumer.as_str(), polls[0].cursor), ("crm", 2));
        assert_eq!(s.backlog(2, 10_000).await.unwrap(), 2);
        assert_eq!(s.backlog(0, 3).await.unwrap(), 3, "capped");

        let f = s.offer_figures(now).await.unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0].last_cursor, f[0].last_5_minutes, f[0].last_24_hours),
            (4, 3, 4)
        );

        s.record_failure(now).await.unwrap();
        s.record_failure(now).await.unwrap();
        s.record_failure(now - Duration::days(3)).await.unwrap();
        assert_eq!(
            s.failed_attempts(now - Duration::hours(24)).await.unwrap(),
            2
        );
        s.purge(std::time::Duration::from_secs(30 * 86_400))
            .await
            .unwrap();
        assert_eq!(
            s.failed_attempts(now - Duration::days(10)).await.unwrap(),
            2,
            "old counters purged"
        );
    })
    .await;
}
