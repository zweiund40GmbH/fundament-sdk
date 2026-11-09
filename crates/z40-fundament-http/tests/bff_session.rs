//! Session lookup against the fake provider: idle and absolute timeout, recording of the last use, the
//! optional extractor.
#![cfg(feature = "test-util")]
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode, header};
use chrono::{TimeDelta, Utc};
use common::app::{TestApp, text};
use z40_fundament_http::bff::TokenHash;

/// The hash of a `name=value` cookie.
fn hash(cookie: &str) -> TokenHash {
    TokenHash::of(cookie.split_once('=').unwrap().1)
}

/// `GET /api/me` with `cookies`.
async fn me(app: &TestApp, cookies: &str) -> axum::response::Response {
    app.send(Method::GET, "/api/me", cookies, &[], "").await
}

#[tokio::test]
async fn expired_sessions_are_rejected_and_deleted() {
    let app = TestApp::start(&[
        ("AUTH_SESSION_IDLE_SECS", "600"),
        ("AUTH_SESSION_MAX_SECS", "3600"),
    ])
    .await;
    let s = app.login_as("u", None, "").await;
    assert_eq!(me(&app, &s).await.status(), StatusCode::OK);
    // Idle: no request for longer than the idle timeout.
    app.store.edit(&hash(&s), |r| {
        r.last_seen_at = Utc::now() - TimeDelta::seconds(601)
    });
    let res = me(&app, &s).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert!(text(res).await.contains("UNAUTHENTICATED"));
    assert_eq!(app.store.session_count(), 0, "deleted");
    // Absolute: active, but the login is older than the absolute timeout.
    let s = app.login_as("u", None, "").await;
    app.store.edit(&hash(&s), |r| {
        r.created_at = Utc::now() - TimeDelta::seconds(3601)
    });
    assert_eq!(me(&app, &s).await.status(), StatusCode::UNAUTHORIZED);
    // A use is recorded when due, which moves the end.
    let s = app.login_as("u", None, "").await;
    let before = Utc::now() - TimeDelta::seconds(120);
    app.store.edit(&hash(&s), |r| r.last_seen_at = before);
    assert_eq!(me(&app, &s).await.status(), StatusCode::OK);
    let rec = app
        .store
        .sessions
        .lock()
        .unwrap()
        .get(&hash(&s))
        .cloned()
        .unwrap();
    assert!(rec.last_seen_at > before && rec.expires_at > before + TimeDelta::seconds(600));
    // Garbage cookies and the optional extractor.
    assert_eq!(
        me(&app, "__Host-crm_sid=x").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let maybe = app.send(Method::GET, "/api/maybe", "", &[], "").await;
    assert_eq!(text(maybe).await, "no");
    let maybe = app.send(Method::GET, "/api/maybe", &s, &[], "").await;
    assert_eq!(text(maybe).await, "yes");
}
