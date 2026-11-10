//! The subgraph endpoint with a small async-graphql federation schema: entities, `_service`, no
//! introspection, the document pre-check, the representation limit and the read limits. No database.
#![cfg(feature = "graphql")]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

use async_graphql::{Context, EmptyMutation, EmptySubscription, ID, Object, Schema, SimpleObject};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tower::ServiceExt;
use z40_fundament::credentials::{KeyList, key_entry};
use z40_fundament::graphql::{ReadBudget, SubgraphEndpoint, SubgraphLimits};
use z40_fundament::protocol::GRAPHQL_PATH;

/// A federation entity.
#[derive(SimpleObject)]
struct Site {
    id: ID,
    name: String,
}

/// A type that nests without end, for the nesting limit.
struct Node;

#[Object]
impl Node {
    async fn name(&self) -> &str {
        "n"
    }
    async fn child(&self) -> Node {
        Node
    }
}

/// Reads running now and the most at once, shared by all requests of a test.
#[derive(Default)]
struct Probe {
    now: AtomicUsize,
    peak: AtomicUsize,
}

struct Query;

#[Object]
impl Query {
    async fn node(&self) -> Node {
        Node
    }

    /// One "storage read" per representation, through the read budget.
    #[graphql(entity)]
    async fn site_by_id(&self, ctx: &Context<'_>, id: ID) -> async_graphql::Result<Site> {
        let probe = ctx.data::<Arc<Probe>>()?;
        ReadBudget::from_ctx(ctx)?
            .read(async {
                let n = probe.now.fetch_add(1, Ordering::SeqCst) + 1;
                probe.peak.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(2)).await;
                probe.now.fetch_sub(1, Ordering::SeqCst);
            })
            .await?;
        Ok(Site {
            name: format!("Site {}", id.as_str()),
            id,
        })
    }
}

fn router(limits: SubgraphLimits, probe: Arc<Probe>) -> Router {
    let keys = KeyList::parse(&key_entry("router", "good-key").unwrap()).unwrap();
    let schema = limits
        .apply(Schema::build(Query, EmptyMutation, EmptySubscription))
        .data(probe)
        .finish();
    SubgraphEndpoint::new(keys, move |req, _budget| {
        let schema = schema.clone();
        async move { schema.execute(req).await }
    })
    .limits(limits)
    .router()
}

fn default_router() -> Router {
    router(SubgraphLimits::default(), Arc::default())
}

async fn post(router: &Router, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(GRAPHQL_PATH)
        .header(header::AUTHORIZATION, "Bearer good-key")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn reps(n: usize) -> Value {
    json!(
        (0..n)
            .map(|i| json!({ "__typename": "Site", "id": i.to_string() }))
            .collect::<Vec<_>>()
    )
}

const ENTITIES: &str =
    "query($r: [_Any!]!) { _entities(representations: $r) { ... on Site { name } } }";

fn code(body: &Value) -> &str {
    body["errors"][0]["extensions"]["code"]
        .as_str()
        .unwrap_or("")
}

#[tokio::test]
async fn service_sdl_works_and_introspection_is_off() {
    let r = default_router();
    let (status, body) = post(&r, json!({ "query": "{ _service { sdl } }" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["data"]["_service"]["sdl"]
            .as_str()
            .unwrap()
            .contains("type Site")
    );
    let (status, body) = post(&r, json!({ "query": "{ __schema { types { name } } }" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["__schema"].is_null(), "{body}");
    let (_, body) = post(
        &r,
        json!({ "query": "{ __type(name: \"Site\") { name } }" }),
    )
    .await;
    assert!(body["data"]["__type"].is_null(), "{body}");
}

#[tokio::test]
async fn mutations_are_impossible_with_empty_mutation() {
    let (status, body) = post(&default_router(), json!({ "query": "mutation { x }" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["errors"][0]["message"].is_string(), "{body}");
    assert!(body.get("data").is_none_or(Value::is_null), "{body}");
}

#[tokio::test]
async fn entities_up_to_the_representation_limit() {
    let r = default_router();
    let (status, body) = post(
        &r,
        json!({ "query": ENTITIES, "variables": { "r": reps(100) } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["_entities"].as_array().unwrap().len(), 100);
    assert_eq!(body["data"]["_entities"][7]["name"], "Site 7");

    let (status, body) = post(
        &r,
        json!({ "query": ENTITIES, "variables": { "r": reps(101) } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "VALIDATION");

    // The same fragment spread twice: 2 × 51 representations.
    let twice = "query($r: [_Any!]!) { ...F ...F } \
                 fragment F on Query { _entities(representations: $r) { ... on Site { name } } }";
    let (status, _) = post(
        &r,
        json!({ "query": twice, "variables": { "r": reps(51) } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `n` fragments, each spreading the next one twice.
fn fan_out(n: usize) -> String {
    let mut q = String::from("query { ...F0 }\n");
    for i in 0..n {
        let body = if i + 1 < n {
            format!("a: node {{ name }} ...F{} ...F{}", i + 1, i + 1)
        } else {
            "node { name }".to_string()
        };
        q.push_str(&format!("fragment F{i} on Query {{ {body} }}\n"));
    }
    q
}

#[tokio::test]
async fn fan_out_and_cycles_are_rejected_quickly_and_the_next_request_is_served() {
    let r = default_router();
    let start = Instant::now();
    for q in [
        fan_out(30),
        fan_out(40),
        "{ ...A } fragment A on Query { ...A ...A }".to_string(),
        "{ ...A } fragment A on Query { node { name } ...B } fragment B on Query { ...A }"
            .to_string(),
        "{ node { name } } fragment A on Query { ...B } fragment B on Query { ...A }".to_string(),
    ] {
        let (status, body) = post(&r, json!({ "query": q })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&body), "VALIDATION");
        assert_eq!(body["errors"][0]["message"], "Ungültige Anfrage.");
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    let (status, body) = post(&r, json!({ "query": "{ node { name } }" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["node"]["name"], "n");
}

/// `node` and `n - 1` times `child` below the operation, then `name`.
fn nested(n: usize) -> String {
    format!(
        "{{ node {{ {}name{} }} }}",
        "child { ".repeat(n - 1),
        " }".repeat(n - 1)
    )
}

#[tokio::test]
async fn nesting_31_runs_and_32_is_rejected() {
    let r = router(SubgraphLimits::default().depth(64), Arc::default());
    let (status, body) = post(&r, json!({ "query": nested(31) })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("errors").is_none(), "{body}");
    let (status, body) = post(&r, json!({ "query": nested(32) })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "VALIDATION");
}

#[tokio::test]
async fn reads_beyond_the_budget_fail_their_field() {
    let r = router(SubgraphLimits::default().reads(3), Arc::default());
    let (status, body) = post(
        &r,
        json!({ "query": ENTITIES, "variables": { "r": reps(5) } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let errors = body["errors"].as_array().unwrap();
    assert!(!errors.is_empty(), "{body}");
    assert!(
        errors
            .iter()
            .all(|e| e["extensions"]["code"] == "VALIDATION"),
        "{body}"
    );
    assert_eq!(
        errors[0]["message"],
        "Zu viele Lesezugriffe in einer Anfrage."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_most_three_reads_run_at_once_across_requests() {
    let probe = Arc::new(Probe::default());
    let r = router(SubgraphLimits::default(), Arc::clone(&probe));
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let r = r.clone();
        tasks.push(tokio::spawn(async move {
            post(
                &r,
                json!({ "query": ENTITIES, "variables": { "r": reps(40) } }),
            )
            .await
        }));
    }
    for t in tasks {
        let (status, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["_entities"].as_array().unwrap().len(), 40);
    }
    assert_eq!(probe.peak.load(Ordering::SeqCst), 3);
}
