//! Request ids and the trace layer: passthrough, replacement, generation, and what the logs never contain.
#![cfg(all(feature = "request-id", feature = "trace"))]
#![allow(clippy::unwrap_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use z40_fundament_http::request_id::{HEADER, RequestId, RequestIdLayer};
use z40_fundament_http::trace;

fn app() -> Router {
    Router::new()
        .route(
            "/api/orders/{id}",
            get(|axum::Extension(id): axum::Extension<RequestId>| async move { id.as_str().to_owned() }),
        )
        .route("/api/login", post(|body: String| async move { body.len().to_string() }))
        .layer(trace::layer())
        .layer(RequestIdLayer)
}

async fn send(req: Request<Body>) -> Response {
    app().oneshot(req).await.unwrap()
}

async fn text(res: Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn valid_id_passes_through() {
    let req = Request::get("/api/orders/1")
        .header(HEADER, "proxy:abc-123")
        .body(Body::empty())
        .unwrap();
    let res = send(req).await;
    assert_eq!(res.headers()[HEADER], "proxy:abc-123");
    assert_eq!(text(res).await, "proxy:abc-123", "handler sees the same id");
}

#[tokio::test]
async fn invalid_id_is_replaced_and_missing_id_generated() {
    for incoming in [Some("bad id {}"), Some(&*"a".repeat(65)), None] {
        let mut b = Request::get("/api/orders/1");
        if let Some(v) = incoming {
            b = b.header(HEADER, v);
        }
        let res = send(b.body(Body::empty()).unwrap()).await;
        let echoed = res.headers()[HEADER].to_str().unwrap().to_owned();
        assert_ne!(Some(echoed.as_str()), incoming);
        assert_eq!(echoed.len(), 36, "a UUID");
        assert_eq!(text(res).await, echoed);
    }
}

/// A `Write` into a shared buffer for the test subscriber.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn trace_never_logs_query_headers_or_bodies() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let get = Request::get("/api/orders/order-77?code=oauth-code-secret&state=st4te")
        .header(HEADER, "trace-id-1")
        .header("authorization", "Bearer bearer-secret")
        .header("cookie", "session=cookie-secret")
        .header("x-api-key", "key-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(get).await.status(), StatusCode::OK);
    let post = Request::post("/api/login")
        .body(Body::from("password=body-secret"))
        .unwrap();
    assert_eq!(send(post).await.status(), StatusCode::OK);
    let missing = Request::get("/nowhere/x?token=query-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(missing).await.status(), StatusCode::NOT_FOUND);

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("route=/api/orders/{id}"), "{logs}");
    assert!(logs.contains("request_id=trace-id-1"), "{logs}");
    assert!(
        logs.contains("method=POST") && logs.contains("route=/api/login"),
        "{logs}"
    );
    assert!(
        logs.contains("status=200") && logs.contains("latency="),
        "{logs}"
    );
    assert!(
        logs.contains("route=/nowhere/x") && logs.contains("status=404"),
        "{logs}"
    );
    for secret in [
        "order-77",
        "oauth-code-secret",
        "st4te",
        "bearer-secret",
        "cookie-secret",
        "key-secret",
        "body-secret",
        "query-secret",
        "?",
    ] {
        assert!(!logs.contains(secret), "{secret} leaked: {logs}");
    }
}

#[tokio::test]
async fn unsafe_incoming_id_is_not_logged_without_the_request_id_layer() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let app = Router::new()
        .route("/", get(|| async { "" }))
        .layer(trace::layer());
    let req = Request::get("/")
        .header(HEADER, "x\"injected=1")
        .body(Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap();
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("finished processing request"), "{logs}");
    assert!(!logs.contains("injected"), "{logs}");
}
