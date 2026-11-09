//! Integration tests of `MySqlSource` against a real MariaDB (`TEST_DATABASE_URL`): outbox in the app's
//! transaction, cursor assignment after commit, retention and the poll log.
#![cfg(feature = "feed-source-mysql")]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

mod common;

use chrono::{Duration, Utc};
use common::source::{T, assign, commit, commit_all, feed, key, source, state};
use common::with_schema;
use serde_json::json;
use z40_fundament::feed_source::{AssignOutcome, FeedSourceStore, SourceStatusStore};
use z40_fundament::feed_source_mysql::{
    InvalidEvent, OutboxEvent, OutboxWriter, SourceTables, WriteEventError,
};

#[tokio::test]
async fn outbox_lives_in_the_app_transaction() {
    let tables = SourceTables::with_prefix("o_").unwrap();
    with_schema("src_outbox", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        let mut tx = s.pool().begin().await.unwrap();
        s.write_event(&mut tx, &state(1, 1)).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(assign(&s).await, AssignOutcome::Assigned(0));
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
        assert_eq!(assign(&s).await, AssignOutcome::Assigned(2));
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
            matches!(
                s.write_event(&mut tx, &state(1, 1)).await,
                Err(WriteEventError::VersionExists)
            ),
            "a version is written once"
        );
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
        assign(&s).await;
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
        assign(&s).await;
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
        assign(&s).await;
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

        s.record_failures(1, now).await.unwrap();
        s.record_failures(1, now).await.unwrap();
        s.record_failures(5, now - Duration::days(3)).await.unwrap();
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

/// The retention walks the outbox in windows of ids: a window without anything to delete does not end
/// it, and later windows continue after the last id (keyset).
#[tokio::test]
async fn retention_walks_all_windows() {
    let tables = SourceTables::with_prefix("w_").unwrap();
    with_schema("src_windows", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        let old = Utc::now() - Duration::days(40);
        // 1500 recent versions of record 1 (nothing to delete), then 1500 old versions of record 2.
        commit_all(&s, (1..=1500).map(|v| state(1, v))).await;
        commit_all(&s, (1..=1500).map(|v| state(2, v).at(old))).await;
        while assign(&s).await != AssignOutcome::Assigned(0) {}
        let gone = s
            .purge(std::time::Duration::from_secs(30 * 86_400))
            .await
            .unwrap();
        assert_eq!(
            gone, 1499,
            "all superseded old states, also in the last windows"
        );
        let rest = feed(&s).await;
        assert_eq!(rest.len(), 1501);
        assert_eq!(rest.last().map(|(k, _)| k.clone()), Some(key(2)));
    })
    .await;
}

/// The writer needs no pool; without an explicit time the database clock stamps the event, `.at` wins.
#[tokio::test]
async fn writer_without_pool_and_database_clock() {
    let tables = SourceTables::with_prefix("c_").unwrap();
    with_schema("src_writer", tables.schema(), |pool| async move {
        let writer = OutboxWriter::new(tables.clone(), [T]).unwrap();
        assert_eq!(source(pool.clone(), tables).writer(), &writer);
        let fixed = chrono::DateTime::parse_from_rfc3339("2020-01-02T03:04:05Z")
            .unwrap()
            .to_utc();
        let mut tx = pool.begin().await.unwrap();
        writer.write(&mut tx, &state(1, 1)).await.unwrap();
        writer.write(&mut tx, &state(2, 1).at(fixed)).await.unwrap();
        let unknown = OutboxEvent::state("urn:example:crm:site", key(3), 1, json!({}));
        assert!(matches!(
            writer.write(&mut tx, &unknown).await,
            Err(WriteEventError::Invalid(InvalidEvent::UnknownType))
        ));
        tx.commit().await.unwrap();
        let (skew, at): (i64, chrono::NaiveDateTime) = sqlx::query_as(
            "SELECT ABS(TIMESTAMPDIFF(SECOND, \
               (SELECT occurred_at FROM c_fundament_outbox WHERE version = 1 AND record_key = ?), \
               UTC_TIMESTAMP(3))), \
             (SELECT occurred_at FROM c_fundament_outbox WHERE record_key = ?)",
        )
        .bind(key(1))
        .bind(key(2))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(skew <= 5, "database clock, {skew} s off");
        assert_eq!(at.and_utc(), fixed, "an explicit time wins");
    })
    .await;
}

/// Two transactions writing the same version: while the first is open, the second waits and gets a
/// retryable lock wait timeout; after the first committed, the version exists.
#[tokio::test]
async fn lock_wait_is_retryable_and_duplicate_version_is_not() {
    let tables = SourceTables::with_prefix("l_").unwrap();
    with_schema("src_lock_wait", tables.schema(), |pool| async move {
        let writer = OutboxWriter::new(tables, [T]).unwrap();
        let mut a = pool.begin().await.unwrap();
        writer.write(&mut a, &state(1, 1)).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("SET SESSION innodb_lock_wait_timeout = 1")
            .execute(&mut *conn)
            .await
            .unwrap();
        let mut b = sqlx::Connection::begin(&mut *conn).await.unwrap();
        let Err(WriteEventError::Store(e)) = writer.write(&mut b, &state(1, 1)).await else {
            panic!("expected a lock wait timeout");
        };
        assert!(e.is_retryable(), "{e}");
        b.rollback().await.unwrap();
        a.commit().await.unwrap();
        let mut c = sqlx::Connection::begin(&mut *conn).await.unwrap();
        let again = writer.write(&mut c, &state(1, 1)).await.unwrap_err();
        assert!(matches!(again, WriteEventError::VersionExists));
        assert!(!again.is_retryable());
    })
    .await;
}
