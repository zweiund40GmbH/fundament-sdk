//! Tests of the endpoint: order of the checks and the shapes of the answers.

use super::*;
use crate::credentials::{MAX_FAILURES, key_entry};
use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request as HttpRequest, header};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

struct Query;

#[Object]
impl Query {
    /// Who called (from the request data).
    async fn caller(&self, ctx: &async_graphql::Context<'_>) -> String {
        ctx.data_opt::<SubgraphCaller>()
            .map_or("?", |c| c.name())
            .to_string()
    }
}

/// The future of the test executor.
type BoxFuture = std::pin::Pin<Box<dyn Future<Output = async_graphql::Response> + Send>>;
/// The test executor, boxed so helpers can name the endpoint type.
type Exec = Box<dyn Fn(async_graphql::Request, ReadBudget) -> BoxFuture + Send + Sync>;

fn endpoint() -> SubgraphEndpoint<Exec> {
    let keys = KeyList::parse(&key_entry("router", "good-key").unwrap()).unwrap();
    let schema = SubgraphLimits::default()
        .apply(Schema::build(Query, EmptyMutation, EmptySubscription))
        .finish();
    let exec: Exec = Box::new(move |req, _budget| {
        let schema = schema.clone();
        Box::pin(async move { schema.execute(req).await })
    });
    SubgraphEndpoint::new(keys, exec)
}

fn request(key: Option<&str>, body: impl Into<Body>) -> HttpRequest<Body> {
    let mut b = HttpRequest::builder()
        .method(Method::POST)
        .uri(GRAPHQL_PATH)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    b.body(body.into()).unwrap()
}

async fn send(
    router: &Router,
    req: HttpRequest<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

fn code(body: &Value) -> &str {
    body["errors"][0]["extensions"]["code"]
        .as_str()
        .unwrap_or("")
}

#[tokio::test]
async fn a_valid_request_runs_with_the_caller_in_the_data() {
    let router = endpoint().router();
    let (status, headers, body) = send(
        &router,
        request(Some("good-key"), r#"{"query":"{ caller }"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["caller"], "router");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn the_key_is_checked_before_the_body_is_read() {
    let router = endpoint().router();
    let big = vec![b'x'; 70 * 1024];
    let (status, headers, body) = send(&router, request(None, big.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(code(&body), "UNAUTHENTICATED");
    assert_eq!(body["errors"][0]["message"], "Anmeldung erforderlich.");
    // With the key the same body is too large.
    let (status, headers, body) = send(&router, request(Some("good-key"), big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&body), "VALIDATION");
    assert_eq!(body["errors"][0]["message"], "Anfrage zu groß.");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn the_brake_answers_429_after_the_failures_and_calls_the_hook() {
    let failed = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&failed);
    let brake = Arc::new(FailureBrake::new());
    let router = endpoint()
        .brake(Arc::clone(&brake))
        .on_failed_attempt(move || {
            f.fetch_add(1, Ordering::Relaxed);
        })
        .router();
    for _ in 0..MAX_FAILURES {
        let (status, _, _) = send(&router, request(Some("wrong"), "{}")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, headers, body) = send(&router, request(Some("good-key"), "{}")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers[header::RETRY_AFTER], "60");
    assert_eq!(code(&body), "RATE_LIMITED");
    assert_eq!(
        failed.load(Ordering::Relaxed),
        MAX_FAILURES as usize,
        "braked requests are not counted"
    );
}

#[tokio::test]
async fn unreadable_requests_are_400_bad_request() {
    let router = endpoint().router();
    for body in ["not json", r#"[{"query":"{ caller }"}]"#, r#"{"query": 5}"#] {
        let (status, _, b) = send(&router, request(Some("good-key"), body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(code(&b), "BAD_REQUEST");
        assert_eq!(b["errors"][0]["message"], "Ungültige Anfrage.");
    }
}

#[tokio::test]
async fn a_rejected_document_is_400_validation_and_a_syntax_error_is_the_engines() {
    let router = endpoint().router();
    let cyclic = r#"{"query":"{ ...A } fragment A on Query { ...A }"}"#;
    let (status, _, b) = send(&router, request(Some("good-key"), cyclic)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&b), "VALIDATION");
    assert_eq!(b["errors"][0]["message"], "Ungültige Anfrage.");

    let (status, _, b) = send(
        &router,
        request(Some("good-key"), r#"{"query":"{ caller"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(b["errors"][0]["message"].is_string());
    assert!(b.get("data").is_none_or(Value::is_null));
}

#[tokio::test]
async fn only_post_is_routed_and_limits_rebuild_the_limiter() {
    let router = endpoint().router();
    let req = HttpRequest::builder()
        .uri(GRAPHQL_PATH)
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);

    let ep = endpoint().limits(SubgraphLimits::default().concurrent_reads(5));
    assert_eq!(ep.read_limiter().available(), 5);
    let shared = ReadLimiter::new(2);
    let ep = endpoint().limiter(shared).limits(SubgraphLimits::default());
    assert_eq!(
        ep.read_limiter().available(),
        2,
        "an explicit limiter is kept"
    );
}
