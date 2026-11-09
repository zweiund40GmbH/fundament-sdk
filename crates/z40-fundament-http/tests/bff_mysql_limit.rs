//! `MySqlLoginLimitStore` against MariaDB (`TEST_DATABASE_URL`): atomic counters, the login limit shared
//! by two instances (per address and in total, so both derive the same address key), its cleanup, and a
//! failing store.
#![cfg(all(feature = "bff-mysql", feature = "test-util"))]
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use chrono::{TimeDelta, Utc};
use common::app::{TestApp, User};
use common::db::with_schema;
use sqlx::MySqlPool;
use z40_fundament_http::bff::{BffBuilder, LoginLimitStore, SessionStore, TokenHash};
use z40_fundament_http::bff_mysql::{MySqlLoginLimitStore, MySqlSessionStore, SessionTables};

fn tables() -> SessionTables {
    SessionTables::with_prefix("app_").unwrap()
}

/// `GET /api/auth/login` of `app` from `ip`: status and `Retry-After`.
async fn start<S: SessionStore + Clone>(
    app: &TestApp<S>,
    ip: &str,
) -> (StatusCode, Option<String>) {
    let headers = [("x-test-ip", ip)];
    let res = app
        .send(Method::GET, "/api/auth/login", "", &headers, "")
        .await;
    let wait = res.headers().get("retry-after");
    (res.status(), wait.map(|v| v.to_str().unwrap().to_owned()))
}

/// Waits for the next minute when the current one ends within 10 s, so a test's starts share one
/// window of the shared login limit.
fn fresh_window() {
    let second = u64::from(chrono::Timelike::second(&Utc::now()));
    if second >= 50 {
        std::thread::sleep(std::time::Duration::from_secs(61 - second));
    }
}

/// Two instances over one session store and one shared login limit with `per_ip` and `total`.
async fn two_instances(
    pool: MySqlPool,
    per_ip: &str,
    total: &str,
) -> [TestApp<MySqlSessionStore>; 2] {
    let sessions = MySqlSessionStore::new(pool, tables());
    let vars = [
        ("AUTH_LOGIN_RATE_PER_IP", per_ip),
        ("AUTH_LOGIN_RATE_TOTAL", total),
    ];
    let shared = |b: BffBuilder<User>| b.login_limit_store(sessions.login_limit_store());
    fresh_window();
    let a = TestApp::custom(sessions.clone(), &vars, shared).await;
    let b = TestApp::custom(sessions.clone(), &vars, shared).await;
    [a, b]
}

async fn count(pool: &MySqlPool, table: &str) -> i64 {
    let sql = sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{table}`"));
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn login_limit_total_is_shared_by_instances() {
    with_schema(
        "bff_limit_total",
        tables().schema(),
        |pool: MySqlPool| async move {
            let [a, b] = two_instances(pool.clone(), "100", "3").await;
            for (app, ip) in [(&a, "192.0.2.1"), (&b, "192.0.2.2"), (&a, "2001:db8::1")] {
                assert_eq!(start(app, ip).await.0, StatusCode::SEE_OTHER, "{ip}");
            }
            let refused = start(&b, "192.0.2.9").await;
            assert_eq!(
                refused,
                (StatusCode::TOO_MANY_REQUESTS, Some("60".to_owned()))
            );
            for i in 0..50 {
                let app = if i % 2 == 0 { &a } else { &b };
                let ip = format!("198.51.100.{i}");
                assert_eq!(start(app, &ip).await.0, StatusCode::TOO_MANY_REQUESTS);
            }
            assert_eq!(
                count(&pool, "app_bff_login").await,
                3,
                "refused starts write no login"
            );

            let rows: Vec<(Vec<u8>, u64)> =
                sqlx::query_as("SELECT `key_hash`, `count` FROM `app_bff_login_limit`")
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                rows.len(),
                3 + 1,
                "three addresses and the total; refused by the total: no row"
            );
            assert!(rows.iter().all(|(k, _)| k.len() == 32), "only hashes");
            let plain = TokenHash::of("ip:192.0.2.1");
            assert!(
                rows.iter().all(|(k, _)| k[..] != plain.as_bytes()[..]),
                "keyed, not the plain SHA-256 of the address"
            );
            let limits = MySqlSessionStore::new(pool.clone(), tables()).login_limit_store();
            assert_eq!(
                limits.delete_expired(Utc::now()).await.unwrap(),
                0,
                "window still open"
            );
            let later = Utc::now() + TimeDelta::seconds(61);
            assert_eq!(limits.delete_expired(later).await.unwrap(), 4);
        },
    )
    .await;
}

#[tokio::test]
async fn login_limit_per_address_is_shared_by_instances() {
    with_schema(
        "bff_limit_ip",
        tables().schema(),
        |pool: MySqlPool| async move {
            let [a, b] = two_instances(pool.clone(), "2", "100").await;
            assert_eq!(start(&a, "2001:db8::1").await.0, StatusCode::SEE_OTHER);
            assert_eq!(
                start(&b, "2001:db8::2").await.0,
                StatusCode::SEE_OTHER,
                "same /64"
            );
            assert_eq!(
                start(&a, "2001:db8::3").await.0,
                StatusCode::TOO_MANY_REQUESTS
            );
            assert_eq!(
                start(&b, "2001:db8::4").await.0,
                StatusCode::TOO_MANY_REQUESTS
            );
            assert_eq!(start(&b, "2001:db8:0:1::1").await.0, StatusCode::SEE_OTHER);
            assert_eq!(count(&pool, "app_bff_login").await, 3);
        },
    )
    .await;
}

#[tokio::test]
async fn failing_login_limit_store_refuses_the_start() {
    with_schema(
        "bff_limit_down",
        tables().schema(),
        |pool: MySqlPool| async move {
            let sessions = MySqlSessionStore::new(pool.clone(), tables());
            let missing = tables().login_limit_table("missing_table").unwrap();
            let down = MySqlLoginLimitStore::new(pool.clone(), missing);
            let app = TestApp::custom(sessions, &[], |b| b.login_limit_store(down)).await;
            let (status, _) = start(&app, "192.0.2.1").await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "fails closed");
            assert_eq!(count(&pool, "app_bff_login").await, 0);
        },
    )
    .await;
}

#[tokio::test]
async fn concurrent_increments_each_get_their_own_count() {
    with_schema(
        "bff_limit_atomic",
        tables().schema(),
        |pool: MySqlPool| async move {
            let limits = MySqlSessionStore::new(pool, tables()).login_limit_store();
            let start = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
            let end = start + TimeDelta::seconds(60);
            let key = TokenHash::of("total");
            let calls = (0..20).map(|_| {
                let limits = limits.clone();
                tokio::spawn(async move { limits.increment(&key, start, end).await.unwrap() })
            });
            let mut counts = Vec::new();
            for call in calls.collect::<Vec<_>>() {
                counts.push(call.await.unwrap());
            }
            counts.sort_unstable();
            assert_eq!(counts, (1..=20).collect::<Vec<u64>>());
            assert_eq!(limits.count(&key, start).await.unwrap(), 20, "read");
            let other = limits.increment(&key, end, end + TimeDelta::seconds(60));
            assert_eq!(other.await.unwrap(), 1, "next window starts at 1");
            let fresh = TokenHash::of("other");
            assert_eq!(
                limits.increment(&fresh, start, end).await.unwrap(),
                1,
                "new row"
            );
            assert_eq!(limits.increment(&fresh, start, end).await.unwrap(), 2);
            let unused = start - TimeDelta::seconds(60);
            assert_eq!(limits.count(&key, unused).await.unwrap(), 0, "no row");
        },
    )
    .await;
}
