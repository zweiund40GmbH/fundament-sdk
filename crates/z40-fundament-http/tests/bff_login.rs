//! Login round trips against the fake provider: cookies, `returnTo`, state and nonce checks, single use,
//! rotation, the login hook and the session timeouts.
#![cfg(feature = "bff")]
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode, header};
use chrono::Utc;
use common::app::{NO_ROLE, TestApp, User, location, session_cookie, set_cookies, text};
use serde_json::json;
use z40_fundament_http::bff::TokenHash;

/// The hash of a `name=value` cookie.
fn hash(cookie: &str) -> TokenHash {
    TokenHash::of(cookie.split_once('=').unwrap().1)
}

#[tokio::test]
async fn login_round_trip_sets_a_strict_host_cookie_and_returns() {
    let app = TestApp::start(&[]).await;
    let (authorize, login_cookie) = app.start_login("/orders/7?tab=items").await;
    assert!(authorize.contains("code_challenge_method=S256"));
    assert!(authorize.contains("scope=openid+profile+email+roles"));
    let a = app
        .provider
        .authorize(&authorize, "user-1", Some("sid-1"), &["admin"], |_| {});
    let res = app
        .send(
            Method::GET,
            &format!("/api/auth/callback?{}", a.query),
            &login_cookie,
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&res), "/orders/7?tab=items");
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let cookies = set_cookies(res.headers());
    assert!(cookies.contains(
        &"__Host-crm_login=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly; Secure".to_owned()
    ));
    let sid_cookie = cookies
        .iter()
        .find(|c| c.starts_with("__Host-crm_sid="))
        .unwrap();
    assert!(
        sid_cookie.ends_with("; Path=/; Max-Age=36000; SameSite=Strict; HttpOnly; Secure"),
        "{sid_cookie}"
    );
    let session = session_cookie(res.headers()).unwrap();
    assert_eq!(session.len(), "__Host-crm_sid=".len() + 43);

    // Only the hash is stored, with sub, sid and the hook's data.
    let stored = app
        .store
        .sessions
        .lock()
        .unwrap()
        .get(&hash(&session))
        .cloned()
        .unwrap();
    assert_eq!(
        (stored.sub.as_str(), stored.sid.as_deref()),
        ("user-1", Some("sid-1"))
    );
    let value = session.split_once('=').unwrap().1;
    assert!(!stored.data.contains(value));
    assert!(
        app.store.logins.lock().unwrap().is_empty(),
        "login state taken"
    );

    let me = app.send(Method::GET, "/api/me", &session, &[], "").await;
    assert_eq!(me.status(), StatusCode::OK);
    let user: User = serde_json::from_str(&text(me).await).unwrap();
    assert_eq!(user.roles, ["admin"]);
    assert_eq!(user.name, "Erika Mustermann");
}

#[tokio::test]
async fn login_cookie_attributes_and_hostile_return_to() {
    let app = TestApp::start(&[]).await;
    for (asked, back) in [
        ("https://evil.example/", "/"),
        ("//evil.example", "/"),
        ("/\\evil", "/"),
    ] {
        let (authorize, login_cookie) = app.start_login(asked).await;
        let a = app
            .provider
            .authorize(&authorize, "u", None, &["admin"], |_| {});
        let res = app
            .send(
                Method::GET,
                &format!("/api/auth/callback?{}", a.query),
                &login_cookie,
                &[],
                "",
            )
            .await;
        assert_eq!(location(&res), back, "{asked}");
    }
    let res = app.send(Method::GET, "/api/auth/login", "", &[], "").await;
    let login = set_cookies(res.headers()).pop().unwrap();
    assert!(
        login.ends_with("; Path=/; Max-Age=600; SameSite=Lax; HttpOnly; Secure"),
        "{login}"
    );
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let stored = app
        .store
        .logins
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&stored.state).contains("pkce"),
        "state is sealed"
    );
}

#[tokio::test]
async fn state_mismatch_is_rejected_and_the_login_is_used_up() {
    let app = TestApp::start(&[]).await;
    let (authorize, login_cookie) = app.start_login("/").await;
    let a = app
        .provider
        .authorize(&authorize, "u", None, &["admin"], |_| {});
    let forged = a.query.replace(&a.state, "forged-state");
    let res = app
        .send(
            Method::GET,
            &format!("/api/auth/callback?{forged}"),
            &login_cookie,
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(session_cookie(res.headers()).is_none());
    assert!(text(res).await.contains("abgelehnt"));
    // The genuine answer no longer works: the login state was taken by the first callback.
    let res = app
        .send(
            Method::GET,
            &format!("/api/auth/callback?{}", a.query),
            &login_cookie,
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(text(res).await.contains("abgelaufen"));
    assert_eq!(app.store.session_count(), 0);
}

#[tokio::test]
async fn nonce_mismatch_is_rejected() {
    let app = TestApp::start(&[]).await;
    let (authorize, login_cookie) = app.start_login("/").await;
    let a = app
        .provider
        .authorize(&authorize, "u", None, &["admin"], |c| {
            c["nonce"] = json!("other-nonce");
        });
    let res = app
        .send(
            Method::GET,
            &format!("/api/auth/callback?{}", a.query),
            &login_cookie,
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(session_cookie(res.headers()).is_none());
    assert_eq!(app.store.session_count(), 0);
}

#[tokio::test]
async fn callback_without_or_with_a_foreign_login_cookie_is_expired() {
    let app = TestApp::start(&[]).await;
    let (authorize, _mine) = app.start_login("/").await;
    let a = app
        .provider
        .authorize(&authorize, "u", None, &["admin"], |_| {});
    let uri = format!("/api/auth/callback?{}", a.query);
    for cookies in [
        "",
        "__Host-crm_login=garbage",
        &format!("__Host-crm_login={}", "A".repeat(43)),
    ] {
        let res = app.send(Method::GET, &uri, cookies, &[], "").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{cookies}");
        assert!(text(res).await.contains("abgelaufen"));
    }
    // An expired login state is refused too.
    let (authorize, login_cookie) = app.start_login("/").await;
    let id = hash(&login_cookie);
    app.store
        .logins
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .expires_at = Utc::now();
    let a = app
        .provider
        .authorize(&authorize, "u", None, &["admin"], |_| {});
    let res = app
        .send(
            Method::GET,
            &format!("/api/auth/callback?{}", a.query),
            &login_cookie,
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hook_refusal_shows_its_text_and_creates_no_session() {
    let app = TestApp::start(&[]).await;
    let (authorize, login_cookie) = app.start_login("/").await;
    let a = app.provider.authorize(&authorize, "u", None, &[], |_| {});
    let res = app
        .send(
            Method::GET,
            &format!("/api/auth/callback?{}", a.query),
            &login_cookie,
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(session_cookie(res.headers()).is_none());
    assert!(text(res).await.contains(NO_ROLE));
    assert_eq!(app.store.session_count(), 0);
}

#[tokio::test]
async fn login_rotates_the_session_id() {
    let app = TestApp::start(&[]).await;
    let first = app.login_as("u", Some("s1"), "").await;
    // A planted (or previous) session cookie is never reused: it is deleted and a new id is issued.
    let second = app.login_as("u", Some("s2"), &first).await;
    assert_ne!(first, second);
    assert!(
        app.store
            .sessions
            .lock()
            .unwrap()
            .get(&hash(&first))
            .is_none()
    );
    assert_eq!(app.store.session_count(), 1);
    let old = app.send(Method::GET, "/api/me", &first, &[], "").await;
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    let planted = format!("__Host-crm_sid={}", "B".repeat(43));
    let third = app.login_as("u", None, &planted).await;
    assert_ne!(third, planted);
}
