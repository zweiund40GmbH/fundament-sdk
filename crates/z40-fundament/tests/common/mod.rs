//! Helpers of the integration tests: a database per test on the server of `TEST_DATABASE_URL`, and a
//! fake feed server on 127.0.0.1.
// Test helpers: each test uses only some of them, and failing fast with unwrap is intended.
#![allow(dead_code, clippy::unwrap_used)]

#[cfg(feature = "feed-source-mysql")]
pub mod source;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use sqlx::{Executor, MySqlPool};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Runs `test` with a pool on a fresh database that has the schema for `tables`, and drops the
/// database afterwards (also when the test panics). Without `TEST_DATABASE_URL` prints `[skip]`.
#[cfg(feature = "replica-mysql")]
pub async fn with_db<F, Fut>(name: &str, tables: z40_fundament::replica_mysql::Tables, test: F)
where
    F: FnOnce(MySqlPool) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    with_schema(name, tables.schema(), test).await;
}

/// Runs `test` with a pool on a fresh database that has `schema` (statements separated by `;`), and
/// drops the database afterwards (also when the test panics). Without `TEST_DATABASE_URL` prints
/// `[skip]`.
pub async fn with_schema<F, Fut>(name: &str, schema: String, test: F)
where
    F: FnOnce(MySqlPool) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let Ok(server) = std::env::var("TEST_DATABASE_URL") else {
        println!("[skip] {name}: TEST_DATABASE_URL not set");
        return;
    };
    let server = server.trim_end_matches('/').to_owned();
    let db = format!("fundament_sdk_{name}_{}", std::process::id());
    let admin = MySqlPool::connect(&server).await.unwrap();
    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS `{db}`"
        )))
        .await
        .unwrap();
    admin
        .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE `{db}`")))
        .await
        .unwrap();
    let url = format!("{server}/{db}");
    // Setup and test run in a task, so a panic in either still drops the database below.
    let result = tokio::spawn(async move {
        let pool = MySqlPool::connect(&url).await.unwrap();
        for stmt in schema_statements(&schema) {
            pool.execute(sqlx::AssertSqlSafe(stmt)).await.unwrap();
        }
        test(pool.clone()).await;
        pool.close().await;
    })
    .await;
    admin
        .execute(sqlx::AssertSqlSafe(format!("DROP DATABASE `{db}`")))
        .await
        .unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// The statements of a schema file (comment lines removed, split at `;`).
fn schema_statements(schema: &str) -> Vec<String> {
    let without_comments: String = schema
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    without_comments
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Answer of the fake feed for a `since`.
pub type Answer = Arc<dyn Fn(u64) -> (StatusCode, String) + Send + Sync>;

/// State of the fake feed: the answer and a delay before answering (milliseconds).
#[derive(Clone)]
struct Feed {
    answer: Answer,
    delay_ms: Arc<AtomicU64>,
}

async fn events(
    State(feed): State<Feed>,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, String) {
    let delay = feed.delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    (feed.answer)(q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// Serves `router` on 127.0.0.1 with the peer address as `ConnectInfo`; returns the base URL.
pub async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    format!("http://{addr}")
}

/// Serves `answer` at `/fundament/v1/events` on 127.0.0.1; returns the base URL.
pub async fn feed_server(answer: Answer) -> String {
    feed_server_delayed(answer, Arc::new(AtomicU64::new(0))).await
}

/// Like [`feed_server`], waiting `delay_ms` (changeable while running) before each answer.
pub async fn feed_server_delayed(answer: Answer, delay_ms: Arc<AtomicU64>) -> String {
    let app = Router::new()
        .route(z40_fundament::protocol::EVENTS_PATH, get(events))
        .with_state(Feed { answer, delay_ms });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}
