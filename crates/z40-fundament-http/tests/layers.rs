//! Security headers, no-store and the CSRF guard on a real axum router.
#![allow(clippy::unwrap_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower::ServiceExt;
use z40_fundament_http::csrf::Csrf;
use z40_fundament_http::no_store::NoStore;
use z40_fundament_http::security_headers::SecurityHeaders;

/// Sends `req` through `app`.
async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
}

fn request(method: Method, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

async fn body(res: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 16)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn security_headers_on_every_response_and_own_csp_kept() {
    let preview = || async {
        let csp = HeaderValue::from_static("default-src 'self'; frame-ancestors 'self'");
        ([(header::CONTENT_SECURITY_POLICY, csp)], "<html>").into_response()
    };
    let app = Router::new()
        .route("/api/me", get(|| async { "{}" }))
        .route("/preview", get(preview))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(SecurityHeaders::api());

    for uri in ["/api/me", "/unknown"] {
        let res = send(&app, request(Method::GET, uri, &[])).await;
        let h = res.headers();
        assert!(
            h[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
        assert!(h.contains_key("permissions-policy"));
        assert_eq!(h["cross-origin-opener-policy"], "same-origin");
        assert_eq!(h["cross-origin-resource-policy"], "same-origin");
        assert!(
            !h.contains_key(header::STRICT_TRANSPORT_SECURITY),
            "HSTS belongs to the edge proxy"
        );
    }
    let res = send(&app, request(Method::GET, "/preview", &[])).await;
    assert!(
        res.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'self'")
    );
    assert_eq!(
        res.headers()[header::X_FRAME_OPTIONS],
        "SAMEORIGIN",
        "derived from the own CSP"
    );
}

#[tokio::test]
async fn csp_per_route_group() {
    let web_csp = "default-src 'self'; script-src 'self'; frame-ancestors 'none'";
    let web = Router::new()
        .route("/", get(|| async { "<html>" }))
        .layer(SecurityHeaders::api().csp(web_csp).unwrap());
    let app = Router::new()
        .route("/api/me", get(|| async { "{}" }))
        .merge(web)
        .layer(SecurityHeaders::api());
    let res = send(&app, request(Method::GET, "/", &[])).await;
    assert_eq!(res.headers()[header::CONTENT_SECURITY_POLICY], web_csp);
    let res = send(&app, request(Method::GET, "/api/me", &[])).await;
    assert_eq!(
        res.headers()[header::CONTENT_SECURITY_POLICY],
        z40_fundament_http::security_headers::API_CSP
    );
}

#[tokio::test]
async fn no_store_on_covered_paths_only() {
    let cacheable = || async { ([(header::CACHE_CONTROL, "max-age=60")], "info") };
    let app = Router::new()
        .route(
            "/api/me",
            get(|| async { ([(header::CACHE_CONTROL, "max-age=600")], "me") }),
        )
        .route("/api/info", get(cacheable))
        .route("/assets/app.js", get(|| async { "js" }))
        .layer(NoStore::new().prefix("/api/").except_path("/api/info"));
    let res = send(&app, request(Method::GET, "/api/me", &[])).await;
    assert_eq!(
        res.headers()[header::CACHE_CONTROL],
        "no-store",
        "replaces the handler's value"
    );
    assert_eq!(res.headers()[header::PRAGMA], "no-cache");
    let res = send(&app, request(Method::GET, "/api/info", &[])).await;
    assert_eq!(res.headers()[header::CACHE_CONTROL], "max-age=60");
    let res = send(&app, request(Method::GET, "/assets/app.js", &[])).await;
    assert!(!res.headers().contains_key(header::CACHE_CONTROL));
}

fn csrf_app(guard: Csrf) -> Router {
    Router::new()
        .route(
            "/api/orders",
            get(|| async { "list" }).post(|| async { "created" }),
        )
        .route("/api/auth/backchannel-logout", post(|| async { "ok" }))
        .route("/fundament/v1/hook", post(|| async { "ok" }))
        .layer(guard)
}

fn guard() -> Csrf {
    Csrf::header("x-requested-with", "crm")
        .unwrap()
        .protect_prefix("/api/")
        .protect_prefix("/fundament/")
        .exempt_path("/api/auth/backchannel-logout")
        .exempt_prefix("/fundament/")
}

#[tokio::test]
async fn csrf_guard() {
    let app = csrf_app(guard());
    let res = send(&app, request(Method::GET, "/api/orders", &[])).await;
    assert_eq!(res.status(), StatusCode::OK, "GET passes");

    let res = send(&app, request(Method::POST, "/api/orders", &[])).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let json = body(res).await;
    assert_eq!(json["code"], "CSRF");
    assert_eq!(json["message"], z40_fundament_http::csrf::MESSAGE);

    let wrong = [("x-requested-with", "XMLHttpRequest")];
    let res = send(&app, request(Method::POST, "/api/orders", &wrong)).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let right = [("x-requested-with", "crm")];
    let res = send(&app, request(Method::POST, "/api/orders", &right)).await;
    assert_eq!(res.status(), StatusCode::OK);

    for exempt in ["/api/auth/backchannel-logout", "/fundament/v1/hook"] {
        let res = send(&app, request(Method::POST, exempt, &[])).await;
        assert_eq!(res.status(), StatusCode::OK, "{exempt} is exempt");
    }
}

#[tokio::test]
async fn csrf_with_own_check_and_rejection() {
    let guard = Csrf::check(|h| h.get("x-csrf").is_some_and(|v| v == "token"))
        .rejection(|| (StatusCode::FORBIDDEN, "custom").into_response());
    let app = csrf_app(guard);
    let res = send(&app, request(Method::DELETE, "/api/orders", &[])).await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "checked before the method is matched"
    );
    let res = send(&app, request(Method::POST, "/api/orders", &[])).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], b"custom");
    let res = send(
        &app,
        request(Method::POST, "/api/orders", &[("x-csrf", "token")]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
}
