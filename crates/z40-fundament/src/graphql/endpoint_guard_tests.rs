//! Tests of the endpoint's guards around the engine: bracket scan, argument limits, deadline and the
//! codes of engine errors.

use super::tests::{code, endpoint, request, send};
use super::*;
use std::time::{Duration, Instant};

/// A JSON request body for `query`.
fn body(query: &str) -> String {
    serde_json::to_string(&json!({ "query": query })).unwrap()
}

#[tokio::test]
async fn deep_brackets_are_400_and_the_next_request_is_served() {
    let router = endpoint().router();
    // 500 levels already overflow a 2 MiB thread in the parser of a debug build, a few thousand in a
    // release build; without the scan this test aborts the test process.
    for n in [500, 1_000, 3_000] {
        assert_deep_is_rejected(&router, n).await;
    }
    let (status, _, b) = send(&router, request(Some("good-key"), body("{ caller }"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["data"]["caller"], "router");
}

/// Lists, objects and a variable default nested `n` deep are refused with `400 VALIDATION`.
async fn assert_deep_is_rejected(router: &Router, n: usize) {
    for q in [
        format!("{{ caller(a: {}1{}) }}", "[".repeat(n), "]".repeat(n)),
        format!("{{ caller(a: {}1{}) }}", "{a:".repeat(n), "}".repeat(n)),
        format!(
            "query($v: Int = {}1{}) {{ caller }}",
            "[".repeat(n),
            "]".repeat(n)
        ),
    ] {
        let (status, _, b) = send(router, request(Some("good-key"), body(&q))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{n}");
        assert_eq!(code(&b), "VALIDATION");
        assert_eq!(b["errors"][0]["message"], "Ungültige Anfrage.");
    }
}

#[tokio::test]
async fn many_arguments_are_refused_within_milliseconds() {
    // Two fragments with the same field and its arguments in opposite order, spread in many sets: the
    // engine would compare every pair of arguments of every pair of fields.
    let n = 4_000;
    let args: Vec<String> = (0..n).map(|i| format!("a{i}:{}", i % 10)).collect();
    let reversed: Vec<String> = args.iter().rev().cloned().collect();
    let sets = "...G ...H ".repeat(45);
    let q = format!(
        "{{ {sets} }} fragment G on Query {{ caller({}) }} fragment H on Query {{ caller({}) }}",
        args.join(","),
        reversed.join(",")
    );
    let b = body(&q);
    assert!(b.len() < 64 * 1024, "within the body limit: {}", b.len());
    let start = Instant::now();
    let (status, _, b) = send(&endpoint().router(), request(Some("good-key"), b)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&b), "VALIDATION");
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn the_deadline_answers_503_and_frees_the_permits() {
    let ep = endpoint().limits(SubgraphLimits::default().timeout(Duration::from_millis(50)));
    let limiter = ep.read_limiter().clone();
    let router = ep.router();
    let start = Instant::now();
    let (status, headers, b) = send(&router, request(Some("good-key"), body("{ slow }"))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&b), "TIMEOUT");
    assert_eq!(b["errors"][0]["message"], "Zeitüberschreitung.");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(
        limiter.available(),
        3,
        "the dropped read gave its permit back"
    );
}

#[tokio::test]
async fn engine_errors_get_validation_and_field_errors_stay() {
    let router = endpoint().router();
    let (status, _, b) = send(&router, request(Some("good-key"), body("{ nope }"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(code(&b), "VALIDATION");
    assert!(b["errors"][0].get("path").is_none());

    let (status, _, b) = send(&router, request(Some("good-key"), body("{ failing }"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["errors"][0]["path"][0], "failing");
    assert_eq!(b["errors"][0]["message"], "kaputt");
    assert!(b["errors"][0].get("extensions").is_none(), "{b}");
}

#[test]
fn a_code_already_set_is_kept() {
    let mut e = async_graphql::ServerError::new("x", None);
    e.extensions
        .get_or_insert_with(Default::default)
        .set("code", "TOO_COMPLEX");
    let mut response = async_graphql::Response::from_errors(vec![e]);
    tag_engine_errors(&mut response);
    let ext = response.errors[0].extensions.as_ref().unwrap();
    assert_eq!(
        ext.get("code"),
        Some(&async_graphql::Value::from("TOO_COMPLEX"))
    );
}
