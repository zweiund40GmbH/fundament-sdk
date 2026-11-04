use super::*;
use crate::credentials::{MAX_FAILURES, key_entry};
use crate::status::StatusBuilder;
use axum::body::{Body, to_bytes};
use axum::http::Request as HttpRequest;
use chrono::Utc;
use serde_json::Value;
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
