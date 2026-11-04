//! The HTTP trace layer: one span per request with `method`, `route` and `request_id`, and an `INFO`
//! event at the end with `status` and `latency` (milliseconds).
//!
//! **What is never recorded:** the query string (OAuth `code` and `state`, search terms travel there),
//! headers (cookies, `Authorization`, keys) and bodies. `route` is the route template of axum
//! (`/api/orders/{id}`), so ids in paths do not end up in the logs either; only a request that matched
//! no route (e.g. a `404`) records its raw path, still without the query.
//!
//! Add the layer with `Router::layer` (it then sees the matched route), inside the
//! [`crate::request_id`] layer:
//!
//! ```
//! # #[cfg(feature = "request-id")] {
//! use axum::{Router, routing::get};
//! use z40_fundament_http::{request_id::RequestIdLayer, trace};
//!
//! let app: Router = Router::new()
//!     .route("/api/orders/{id}", get(|| async { "order" }))
//!     .layer(trace::layer())
//!     .layer(RequestIdLayer);
//! # }
//! ```

use axum::extract::MatchedPath;
use axum::http::Request;
use tower_http::LatencyUnit;
use tower_http::classify::{ServerErrorsAsFailures, SharedClassifier};
use tower_http::trace::{DefaultOnResponse, MakeSpan, TraceLayer};
use tracing::{Level, Span};

/// The trace layer type of [`layer`].
pub type HttpTraceLayer = TraceLayer<SharedClassifier<ServerErrorsAsFailures>, RouteSpan>;

/// [`MakeSpan`] recording method, route template (or raw path without query) and request id.
#[derive(Debug, Clone, Copy, Default)]
pub struct RouteSpan;

impl<B> MakeSpan<B> for RouteSpan {
    /// The `request` span.
    fn make_span(&mut self, req: &Request<B>) -> Span {
        let route = req
            .extensions()
            .get::<MatchedPath>()
            .map_or_else(|| req.uri().path(), MatchedPath::as_str);
        let request_id = req
            .headers()
            .get(crate::request_id::HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|id| crate::request_id::is_acceptable(id))
            .unwrap_or_default();
        tracing::info_span!(
            "request",
            method = %req.method(),
            route = %route,
            request_id = %request_id,
        )
    }
}

/// The trace layer: [`RouteSpan`], the end of each request at `INFO` with status and latency in ms,
/// `5xx` and failures at `ERROR` (tower-http's defaults, without headers).
pub fn layer() -> HttpTraceLayer {
    TraceLayer::new_for_http()
        .make_span_with(RouteSpan)
        .on_response(
            DefaultOnResponse::new()
                .level(Level::INFO)
                .latency_unit(LatencyUnit::Millis),
        )
}
