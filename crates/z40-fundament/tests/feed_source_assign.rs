//! Integration tests of the cursor assigner of `MySqlSource` against a real MariaDB
//! (`TEST_DATABASE_URL`): waiting for open transactions, batches that lock only what they number, and
//! cancellation.
#![cfg(feature = "feed-source-mysql")]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::source::{WAIT, assign, commit, commit_all, feed, key, source, state};
use common::with_schema;
use std::time::{Duration, Instant};
use z40_fundament::feed_source::{AssignOutcome, FeedSourceStore};
use z40_fundament::feed_source_mysql::SourceTables;

#[tokio::test]
async fn open_transaction_is_waited_for_and_never_skipped() {
    let tables = SourceTables::with_prefix("").unwrap();
    with_schema("src_assign", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        commit(&s, state(1, 1)).await;
        assert_eq!(assign(&s).await, AssignOutcome::Assigned(1));

        // A second connection holds an uncommitted event; a later event commits meanwhile.
        let mut open = s.pool().begin().await.unwrap();
        s.write_event(&mut open, &state(2, 1)).await.unwrap();
        commit(&s, state(3, 1)).await;

        let started = Instant::now();
        let (first, second) = tokio::join!(s.assign_cursors(WAIT), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            s.assign_cursors(WAIT).await
        });
        assert_eq!(
            first.unwrap(),
            AssignOutcome::Waiting(0),
            "waits, never skips"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "bounded wait"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            second.unwrap(),
            AssignOutcome::Skipped,
            "one assigner at a time"
        );
        assert_eq!(feed(&s).await, [(key(1), 1)], "the later event waits too");

        open.commit().await.unwrap();
        assert_eq!(assign(&s).await, AssignOutcome::Assigned(2));
        assert_eq!(
            feed(&s).await,
            [(key(1), 1), (key(2), 2), (key(3), 3)],
            "after commit it gets a higher cursor than everything assigned before"
        );
    })
    .await;
}

/// A batch locks only the rows it numbers (index `(cursor, id)`, no filesort): an open transaction
/// whose row comes after 3000 committed ones holds up only the batch that reaches it.
#[tokio::test]
async fn batches_lock_only_what_they_number() {
    let tables = SourceTables::with_prefix("b_").unwrap();
    with_schema("src_batches", tables.schema(), |pool| async move {
        let s = source(pool, tables);
        commit_all(&s, (1..=3000).map(|n| state(n, 1))).await;
        let mut open = s.pool().begin().await.unwrap();
        s.write_event(&mut open, &state(5000, 1)).await.unwrap();
        assert_eq!(assign(&s).await, AssignOutcome::Waiting(3000));
        assert_eq!(feed(&s).await.len(), 3000);
        open.commit().await.unwrap();
        assert_eq!(assign(&s).await, AssignOutcome::Assigned(1));
        assert_eq!(feed(&s).await.last(), Some(&(key(5000), 3001)));
    })
    .await;
}

/// A round cancelled at any point – also while `GET_LOCK` is on its way – leaves no lock behind.
#[tokio::test]
async fn cancelled_round_releases_the_lock() {
    let tables = SourceTables::with_prefix("").unwrap();
    with_schema("src_cancel", tables.schema(), |pool| async move {
        let s = source(pool.clone(), tables);
        let free = || async {
            let f: Option<i64> = sqlx::query_scalar(
                "SELECT IS_FREE_LOCK(LEFT(CONCAT('fundament-cursor', ':', DATABASE()), 64))",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            f == Some(1)
        };
        for micros in [0, 50, 200, 500, 1000, 2000, 5000, 10_000] {
            let _ =
                tokio::time::timeout(Duration::from_micros(micros), s.assign_cursors(WAIT)).await;
            // The cancelled round's connection closes in the background.
            let mut released = false;
            for _ in 0..60 {
                released = free().await;
                if released {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(released, "lock released after cancelling at {micros} µs");
        }
        commit(&s, state(1, 1)).await;
        assert_eq!(assign(&s).await, AssignOutcome::Assigned(1), "not Skipped");
    })
    .await;
}
