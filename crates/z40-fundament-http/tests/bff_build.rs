//! Login limit, settings checked at build time and the error page of a failed login.
#![cfg(feature = "bff")]
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::IntoResponse;
use common::app::{TestApp, User, set_cookies, text};
use common::memory::MemoryStore;
use z40_fundament_http::bff::{Bff, BffBuildError, BffConfig, InvalidUrl, LoginDenied};

/// `GET /api/auth/login` from `ip`.
async fn start(app: &TestApp, ip: &str) -> StatusCode {
    let headers = [("x-test-ip", ip)];
    app.send(Method::GET, "/api/auth/login", "", &headers, "")
        .await
        .status()
}

#[tokio::test]
async fn login_starts_are_limited_per_address_and_in_total() {
    let vars = [
        ("AUTH_LOGIN_RATE_PER_IP", "2"),
        ("AUTH_LOGIN_RATE_TOTAL", "3"),
    ];
    let app = TestApp::start(&vars).await;
    let rows = || app.store.logins.lock().unwrap().len();
    assert_eq!(start(&app, "192.0.2.1").await, StatusCode::SEE_OTHER);
    assert_eq!(start(&app, "192.0.2.1").await, StatusCode::SEE_OTHER);
    let res = app
        .send(
            Method::GET,
            "/api/auth/login",
            "",
            &[("x-test-ip", "192.0.2.1")],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(res.headers()[header::RETRY_AFTER], "60");
    assert!(set_cookies(res.headers()).is_empty(), "no login cookie");
    assert!(text(res).await.contains("Zu viele Anmeldeversuche"));
    assert_eq!(rows(), 2, "the refused start wrote no row");
    // Another address may start until the total is used up.
    assert_eq!(start(&app, "192.0.2.2").await, StatusCode::SEE_OTHER);
    assert_eq!(
        start(&app, "192.0.2.3").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(rows(), 3);
}

#[tokio::test]
async fn login_limit_can_be_set_in_code_or_turned_off() {
    let app = TestApp::custom(MemoryStore::default(), &[], |b| b.login_limit(1, 100)).await;
    assert_eq!(start(&app, "192.0.2.1").await, StatusCode::SEE_OTHER);
    assert_eq!(
        start(&app, "192.0.2.1").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let vars = [("AUTH_LOGIN_RATE_PER_IP", "1")];
    let app = TestApp::custom(MemoryStore::default(), &vars, |b| b.without_login_limit()).await;
    for _ in 0..5 {
        assert_eq!(start(&app, "192.0.2.1").await, StatusCode::SEE_OTHER);
    }
}

fn config() -> BffConfig {
    let env = |k: &str| match k {
        "AUTH_ISSUER" => Some("https://login.example.com".to_owned()),
        "AUTH_CLIENT_ID" => Some("crm".to_owned()),
        "AUTH_CLIENT_SECRET" => Some("s3cr3t".to_owned()),
        "AUTH_REDIRECT_URI" => Some("https://crm.example.com/api/auth/callback".to_owned()),
        _ => None,
    };
    BffConfig::from_lookup("AUTH", env).unwrap()
}

fn build(config: BffConfig) -> Result<Bff<User>, BffBuildError> {
    Bff::builder(config, MemoryStore::default(), |_, _| async {
        Err::<User, _>(LoginDenied::rejected())
    })
    .build()
}

#[test]
fn urls_changed_after_reading_are_refused_at_build() {
    assert!(build(config()).is_ok());
    let mut c = config();
    c.redirect_uri = "http://app.example.com/api/auth/callback".to_owned();
    assert_eq!(
        build(c).unwrap_err(),
        BffBuildError::Url(InvalidUrl("redirect_uri"))
    );
    let mut c = config();
    c.post_logout_redirect_uri = Some("javascript:alert(1)".to_owned());
    assert!(matches!(build(c), Err(BffBuildError::Url(_))));
    let mut c = config();
    c.login_rate_total = 0;
    assert_eq!(build(c).unwrap_err(), BffBuildError::LoginLimit);
}

#[tokio::test]
async fn failed_login_keeps_every_set_cookie() {
    let app = TestApp::custom(MemoryStore::default(), &[], |b| {
        b.error_page(|f| {
            let mut res = (f.status(), "failed").into_response();
            for c in ["page_a=1", "page_b=2"] {
                res.headers_mut()
                    .append(header::SET_COOKIE, HeaderValue::from_static(c));
            }
            res
        })
    })
    .await;
    let res = app
        .send(
            Method::GET,
            "/api/auth/callback?code=x&state=y",
            "",
            &[],
            "",
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let cookies = set_cookies(res.headers());
    assert_eq!(cookies.len(), 3, "{cookies:?}");
    assert!(cookies.iter().any(|c| c.starts_with("__Host-crm_login=;")));
}
