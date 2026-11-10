//! Tests of the endpoint's guards around the engine: bracket scan, argument limits, deadline and the
//! codes of engine errors.

use super::tests::{code, endpoint, request, send};
use super::*;
use serde_json::Value;
use std::time::{Duration, Instant};

/// A JSON request body for `query`.
fn body(query: &str) -> String {
    serde_json::to_string(&json!({ "query": query })).unwrap()
}

/// The test endpoint with a body limit of 64 KiB (four times the default), for documents that only a
/// larger limit lets through to the pre-check.
fn wide_endpoint() -> Router {
    endpoint()
        .limits(SubgraphLimits::default().body(64 * 1024))
        .router()
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

/// Lists, objects, a variable default and lists behind an unterminated block string nested `n` deep are
/// refused with `400 VALIDATION`.
async fn assert_deep_is_rejected(router: &Router, n: usize) {
    for q in [
        format!("{{ caller(a: {}1{}) }}", "[".repeat(n), "]".repeat(n)),
        format!("{{ caller(a: {}1{}) }}", "{a:".repeat(n), "}".repeat(n)),
        format!(
            "query($v: Int = {}1{}) {{ caller }}",
            "[".repeat(n),
            "]".repeat(n)
        ),
        // The parser reads `"""x"` as `""` and `"x"` and parses the brackets behind it.
        format!(
            "{{ caller(a: [\"\"\"x\" {}1{}]) }}",
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
    // engine compares the arguments of fields with the same response name, quadratic in the number of
    // arguments per field.
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
    let (status, _, b) = send(&wide_endpoint(), request(Some("good-key"), b)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&b), "VALIDATION");
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn a_large_literal_spread_many_times_is_refused_quickly() {
    // 900 instances of an object with 6 000 keys: the engine would convert it at every instance, for
    // more than a second of synchronous work that no deadline stops.
    let object: Vec<String> = (0..6_000).map(|i| format!("k{i}:1")).collect();
    let q = format!(
        "{{ {} }} fragment F1 on Query {{ {} }} fragment L on Query {{ caller(a: {{{}}}) }}",
        "...F1 ".repeat(30),
        "...L ".repeat(30),
        object.join(",")
    );
    let b = body(&q);
    assert!(b.len() < 64 * 1024, "within the body limit: {}", b.len());
    let router = wide_endpoint();
    let start = Instant::now();
    let (status, _, b) = send(&router, request(Some("good-key"), b)).await;
    let elapsed = start.elapsed();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&b), "VALIDATION");
    // Reading the 47 KB body and parsing it dominate; unoptimised they take a few dozen milliseconds.
    let bound = if cfg!(debug_assertions) { 300 } else { 50 };
    assert!(elapsed < Duration::from_millis(bound), "{elapsed:?}");
}

#[tokio::test]
async fn a_long_variable_used_at_many_instances_is_refused_quickly() {
    // 10 aliases taking `$s` in a fragment spread 100 times, `$s` a 1 MB string: the engine would copy it
    // at every one of the 1 000 instances, synchronously. An app may allow such a body for record inputs.
    let aliases: Vec<String> = (0..10).map(|i| format!("a{i}: caller(a: $s)")).collect();
    let q = format!(
        "query($s: String) {{ {} }} fragment F on Query {{ {} }}",
        "...F ".repeat(100),
        aliases.join(" ")
    );
    let b =
        serde_json::to_string(&json!({ "query": q, "variables": { "s": "x".repeat(1_000_000) } }))
            .unwrap();
    let router = endpoint()
        .limits(SubgraphLimits::default().body(2 * 1024 * 1024))
        .router();
    let start = Instant::now();
    let (status, _, b) = send(&router, request(Some("good-key"), b)).await;
    let elapsed = start.elapsed();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&b), "VALIDATION");
    // Reading and decoding the 1 MB body dominate.
    let bound = if cfg!(debug_assertions) { 300 } else { 50 };
    assert!(elapsed < Duration::from_millis(bound), "{elapsed:?}");
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

#[tokio::test]
async fn the_field_merge_documents_are_400_within_milliseconds() {
    // Without the pre-check: 0.8 s of synchronous work and an answer of 422 MB (unused fragments), a
    // multiple with a long name, and about 2.4 s for the chain of used fragments.
    let long = "n".repeat(10_000);
    let router = wide_endpoint();
    for q in [
        crate::graphql::merge::tests::unused_fan("m"),
        crate::graphql::merge::tests::unused_fan(&long),
        crate::graphql::merge::tests::used_chain(),
    ] {
        let start = Instant::now();
        let (status, _, b) = send(&router, request(Some("good-key"), body(&q))).await;
        let elapsed = start.elapsed();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&b), "VALIDATION");
        let bound = if cfg!(debug_assertions) { 100 } else { 20 };
        assert!(elapsed < Duration::from_millis(bound), "{elapsed:?}");
    }
}

/// The errors of an answer to a query with `n` unknown fields, written directly or in a fragment.
async fn errors_for(router: &Router, n: usize, in_fragment: bool) -> Vec<Value> {
    let fields: String = (0..n).map(|i| format!("n{i} ")).collect();
    let q = if in_fragment {
        format!("{{ caller ...F }} fragment F on Query {{ {fields} }}")
    } else {
        format!("{{ caller {fields} }}")
    };
    let (status, _, b) = send(router, request(Some("good-key"), body(&q))).await;
    assert_eq!(status, StatusCode::OK);
    b["errors"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn an_answer_carries_at_most_the_error_limit_and_a_note() {
    let router = endpoint().router();
    for n in [1, 19, 20, 21, 57, 500] {
        for in_fragment in [false, true] {
            let errors = errors_for(&router, n, in_fragment).await;
            assert_eq!(errors.len(), n.min(20) + usize::from(n > 20), "{n}");
            assert!(
                errors
                    .iter()
                    .all(|e| e["extensions"]["code"] == "VALIDATION")
            );
            let last = &errors[errors.len() - 1]["message"];
            assert_eq!(last == "Weitere Fehler ausgelassen.", n > 20, "{n}: {last}");
        }
    }
    let router = endpoint()
        .limits(SubgraphLimits::default().errors(3))
        .router();
    assert_eq!(errors_for(&router, 10, false).await.len(), 4);
}
