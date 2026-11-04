//! Helpers of the feed source tests: a `MySqlSource` for one customer type and committed events.

use serde_json::json;
use sqlx::MySqlPool;
use std::time::Duration;
use z40_fundament::feed_source::{AssignOutcome, FeedSourceStore};
use z40_fundament::feed_source_mysql::{MySqlSource, OutboxEvent, SourceTables};

/// The record type of the tests.
pub const T: &str = "urn:example:crm:customer";
/// The wait of the assigner in the tests (short, like the request path).
pub const WAIT: Duration = Duration::from_secs(1);

/// The key of record `n`.
pub fn key(n: u64) -> String {
    format!("0192f39e-0000-7000-8000-{n:012}")
}

/// A source offering [`T`].
pub fn source(pool: MySqlPool, tables: SourceTables) -> MySqlSource {
    MySqlSource::new(pool, tables, [T]).unwrap()
}

/// The state of record `n` in `version`.
pub fn state(n: u64, version: u64) -> OutboxEvent {
    OutboxEvent::state(
        T,
        key(n),
        version,
        json!({ "name": format!("Customer {n}") }),
    )
}

/// Writes `events` in one committed transaction.
pub async fn commit_all(s: &MySqlSource, events: impl IntoIterator<Item = OutboxEvent>) {
    let mut tx = s.pool().begin().await.unwrap();
    for e in events {
        s.write_event(&mut tx, &e).await.unwrap();
    }
    tx.commit().await.unwrap();
}

/// Writes `e` in its own committed transaction.
pub async fn commit(s: &MySqlSource, e: OutboxEvent) {
    commit_all(s, [e]).await;
}

/// One assigner round with [`WAIT`].
pub async fn assign(s: &MySqlSource) -> AssignOutcome {
    s.assign_cursors(WAIT).await.unwrap()
}

/// (key, cursor) of every readable event since 0 (at most 5000).
pub async fn feed(s: &MySqlSource) -> Vec<(String, u64)> {
    let all = s.read(0, 5000, &[T.to_owned()]).await.unwrap();
    all.into_iter().map(|e| (e.key, e.cursor)).collect()
}
