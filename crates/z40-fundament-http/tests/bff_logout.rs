//! Logout and back-channel logout against the fake provider, and the CSRF layer in front of the logout.
#![cfg(feature = "bff")]
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode, header};
use common::app::{TestApp, set_cookies, text};
use url::Url;
use z40_fundament_http::csrf::Csrf;

const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");

#[tokio::test]
async fn logout_ends_the_session_without_a_token_in_the_url() {
    let app = TestApp::start(&[]).await;
    let s = app.login_as("u", Some("sid-1"), "").await;
    let res = app
        .send(Method::POST, "/api/auth/logout", &s, &[], "")
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let cleared = set_cookies(res.headers());
    assert_eq!(
        cleared,
        ["__Host-crm_sid=; Path=/; Max-Age=0; SameSite=Strict; HttpOnly; Secure"]
    );
    let body: serde_json::Value = serde_json::from_str(&text(res).await).unwrap();
    let url = Url::parse(body["logoutUrl"].as_str().unwrap()).unwrap();
    assert!(
        url.as_str()
            .starts_with(&format!("{}/logout?", app.provider.issuer))
    );
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["post_logout_redirect_uri"], "https://crm.example.com/");
    assert_eq!(q["client_id"], "crm");
    assert!(!q.contains_key("id_token_hint"), "no token in a URL");
    assert_eq!(app.store.session_count(), 0);
    let me = app.send(Method::GET, "/api/me", &s, &[], "").await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
    // Without a session the logout still answers (and clears the cookie).
    let res = app
        .send(Method::POST, "/api/auth/logout", "", &[], "")
        .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn logout_started_by_another_site_is_refused() {
    let app = TestApp::start(&[]).await;
    let s = app.login_as("u", None, "").await;
    for site in ["cross-site", "same-site", "none"] {
        let res = app
            .send(
                Method::POST,
                "/api/auth/logout",
                &s,
                &[("sec-fetch-site", site)],
                "",
            )
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{site}");
        assert!(text(res).await.contains("CSRF"));
        assert_eq!(app.store.session_count(), 1, "not logged out ({site})");
    }
    let res = app
        .send(
            Method::POST,
            "/api/auth/logout",
            &s,
            &[("sec-fetch-site", "same-origin")],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(app.store.session_count(), 0);
}

#[tokio::test]
async fn back_channel_logout_with_an_unknown_sid_ends_sessions_without_sid() {
    let app = TestApp::start(&[]).await;
    let plain = app.login_as("erin", None, "").await;
    let other = app.login_as("erin", Some("sid-e2"), "").await;
    let res = post(
        &app,
        app.provider.logout_token(Some("erin"), Some("sid-e1")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        status(&app, &plain).await,
        StatusCode::UNAUTHORIZED,
        "no sid: ended"
    );
    assert_eq!(
        status(&app, &other).await,
        StatusCode::OK,
        "other sid: kept"
    );
    // A matching sid ends only that session, never the ones without sid.
    let plain = app.login_as("erin", None, "").await;
    let res = post(
        &app,
        app.provider.logout_token(Some("erin"), Some("sid-e2")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(status(&app, &other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(&app, &plain).await, StatusCode::OK);
}

#[tokio::test]
async fn logout_is_guarded_by_the_csrf_layer_and_back_channel_is_exempt() {
    let app = TestApp::start(&[]).await;
    let s = app.login_as("u", Some("sid-1"), "").await;
    let guarded = app.router.clone().layer(
        Csrf::header("x-requested-with", "crm")
            .unwrap()
            .exempt_path("/api/auth/backchannel-logout"),
    );
    let send = |headers: Vec<(&'static str, &'static str)>, uri: &'static str, body: String| {
        let mut b = axum::http::Request::builder().method(Method::POST).uri(uri);
        b = b.header(header::COOKIE, s.as_str());
        for (k, v) in headers {
            b = b.header(k, v);
        }
        let req = b.body(axum::body::Body::from(body)).unwrap();
        tower::ServiceExt::oneshot(guarded.clone(), req)
    };
    let res = send(vec![], "/api/auth/logout", String::new())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(app.store.session_count(), 1, "not logged out");
    let token = app.provider.logout_token(None, Some("sid-1"));
    let res = send(
        vec![FORM],
        "/api/auth/backchannel-logout",
        format!("logout_token={token}"),
    )
    .await
    .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(app.store.session_count(), 0);
}

/// Posts `token` to the back-channel logout.
async fn post(app: &TestApp, token: String) -> axum::response::Response {
    let body = format!("logout_token={token}");
    app.send(
        Method::POST,
        "/api/auth/backchannel-logout",
        "",
        &[FORM],
        &body,
    )
    .await
}

/// Status of `GET /api/me` with `cookie`.
async fn status(app: &TestApp, cookie: &str) -> StatusCode {
    app.send(Method::GET, "/api/me", cookie, &[], "")
        .await
        .status()
}

#[tokio::test]
async fn back_channel_logout_by_sid_and_by_sub() {
    let app = TestApp::start(&[]).await;
    let a1 = app.login_as("alice", Some("sid-a1"), "").await;
    let a2 = app.login_as("alice", Some("sid-a2"), "").await;
    let b = app.login_as("bob", Some("sid-b"), "").await;
    // With `sid`: only that session ends.
    let res = post(
        &app,
        app.provider.logout_token(Some("alice"), Some("sid-a1")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(status(&app, &a1).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(&app, &a2).await, StatusCode::OK);
    // Without `sid`: every session of the subject ends.
    let res = post(&app, app.provider.logout_token(Some("alice"), None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(status(&app, &a2).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(&app, &b).await, StatusCode::OK);
    // Invalid tokens: generic 400, nothing ends.
    let mut forged = app.provider.logout_token(Some("bob"), Some("sid-b"));
    forged.push('x');
    let id_token_typ = app.provider.sign(
        "JWT",
        &serde_json::json!({"iss": app.provider.issuer, "aud": "crm", "sid": "sid-b"}),
    );
    for token in [forged, id_token_typ, "a.b.c".to_owned()] {
        let res = post(&app, token).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(text(res).await.is_empty(), "no details");
    }
    let res = app
        .send(
            Method::POST,
            "/api/auth/backchannel-logout",
            "",
            &[("content-type", "application/json")],
            "{}",
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(status(&app, &b).await, StatusCode::OK);
}

#[tokio::test]
async fn end_sessions_of_a_subject() {
    let app = TestApp::start(&[]).await;
    app.login_as("carol", None, "").await;
    app.login_as("carol", None, "").await;
    app.login_as("dave", None, "").await;
    assert_eq!(app.bff.end_sessions_of("carol").await.unwrap(), 2);
    assert_eq!(app.store.session_count(), 1);
}
