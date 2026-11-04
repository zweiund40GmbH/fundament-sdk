use super::*;
use crate::credentials::{MAX_FAILURES, key_entry};
use crate::status::StatusBuilder;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::Request as HttpRequest;
use axum::http::{StatusCode, header};
use chrono::Utc;
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt;

/// A provider answering ok, failing, or hanging.
#[derive(Clone, Copy)]
enum Fake {
    Ok,
    Fails,
    Hangs,
}

impl StatusProvider for Fake {
    async fn status(&self, caller: &str) -> Result<StatusDocument, BoxError> {
        match self {
            Fake::Ok => Ok(StatusBuilder::new("billing", "1.0.0", Utc::now())
                .advisory(crate::protocol::Level::Ok, caller)
                .build()),
            Fake::Fails => Err("database down".into()),
            Fake::Hangs => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Err("late".into())
            }
        }
    }
}

fn router(p: Fake) -> Router {
    let keys = KeyList::parse(&key_entry("monitor", "good-key").unwrap()).unwrap();
    StatusEndpoint::new(p, keys)
        .deadline(Duration::from_millis(50))
        .router()
}

async fn call(r: &Router, auth: Option<&str>, ip: &str) -> (StatusCode, Response) {
    let mut req = HttpRequest::get(STATUS_PATH);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    let mut req = req.body(Body::empty()).unwrap();
    let addr: SocketAddr = format!("{ip}:1234").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let res = r.clone().oneshot(req).await.unwrap();
    (res.status(), res)
}

async fn json_body(res: Response) -> Value {
    serde_json::from_slice(&to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap()
}

#[tokio::test]
async fn valid_key_gets_the_document() {
    let r = router(Fake::Ok);
    let (status, res) = call(&r, Some("Bearer good-key"), "203.0.113.1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let v = json_body(res).await;
    assert_eq!(v["app"], "billing");
    assert_eq!(
        v["hinweise"][0]["text"], "monitor",
        "caller is the key name"
    );
}

#[tokio::test]
async fn wrong_or_missing_key_is_401_then_429() {
    let r = router(Fake::Ok);
    let (status, res) = call(&r, None, "203.0.113.2").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(json_body(res).await["code"], "UNAUTHENTICATED");
    for _ in 1..MAX_FAILURES {
        let (s, _) = call(&r, Some("Bearer wrong"), "203.0.113.2").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (status, res) = call(&r, Some("Bearer good-key"), "203.0.113.2").await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "braked even with the right key"
    );
    assert_eq!(res.headers()[header::RETRY_AFTER], "60");
    assert_eq!(json_body(res).await["code"], "RATE_LIMITED");
    let (other, _) = call(&r, Some("Bearer good-key"), "203.0.113.3").await;
    assert_eq!(other, StatusCode::OK, "other addresses are not braked");
}

#[tokio::test]
async fn provider_failure_and_timeout_are_500() {
    for p in [Fake::Fails, Fake::Hangs] {
        let (status, res) = call(&router(p), Some("Bearer good-key"), "203.0.113.4").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let v = json_body(res).await;
        assert_eq!(v["code"], "INTERNAL");
        assert!(
            !v.to_string().contains("database"),
            "no details to the client"
        );
    }
}

#[tokio::test]
async fn empty_key_list_refuses_everyone() {
    let r = StatusEndpoint::new(Fake::Ok, KeyList::default()).router();
    let (status, _) = call(&r, Some("Bearer good-key"), "203.0.113.5").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A request without `ConnectInfo` and with an optional `X-Forwarded-For`.
async fn call_plain(r: &Router, auth: &str, forwarded: Option<&str>) -> StatusCode {
    let mut req = HttpRequest::get(STATUS_PATH).header(header::AUTHORIZATION, auth);
    if let Some(f) = forwarded {
        req = req.header("x-forwarded-for", f);
    }
    r.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn without_connect_info_all_clients_share_one_brake() {
    let r = router(Fake::Ok);
    for _ in 0..MAX_FAILURES {
        assert_eq!(
            call_plain(&r, "Bearer wrong", None).await,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        call_plain(&r, "Bearer good-key", None).await,
        StatusCode::TOO_MANY_REQUESTS,
        "unknown addresses are counted together"
    );
}

#[tokio::test]
async fn custom_resolver_captures_proxy_configuration() {
    let trusted_header = true;
    let keys = KeyList::parse(&key_entry("monitor", "good-key").unwrap()).unwrap();
    let r: Router = StatusEndpoint::new(Fake::Ok, keys)
        .client_ip(move |parts: &Parts| {
            trusted_header
                .then(|| {
                    parts
                        .headers
                        .get("x-forwarded-for")?
                        .to_str()
                        .ok()?
                        .parse()
                        .ok()
                })
                .flatten()
        })
        .router();
    for _ in 0..MAX_FAILURES {
        call_plain(&r, "Bearer wrong", Some("198.51.100.7")).await;
    }
    assert_eq!(
        call_plain(&r, "Bearer good-key", Some("198.51.100.7")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        call_plain(&r, "Bearer good-key", Some("198.51.100.8")).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn failed_attempts_reach_the_hook() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let count = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&count);
    let keys = KeyList::parse(&key_entry("monitor", "good-key").unwrap()).unwrap();
    let r: Router = StatusEndpoint::new(Fake::Ok, keys)
        .on_failed_attempt(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .router();
    for _ in 0..(MAX_FAILURES + 3) {
        call(&r, Some("Bearer wrong"), "203.0.113.9").await;
    }
    call(&r, Some("Bearer good-key"), "203.0.113.10").await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        u64::from(MAX_FAILURES),
        "braked ones are not counted"
    );
}
