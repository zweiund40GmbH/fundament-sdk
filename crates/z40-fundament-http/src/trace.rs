//! The HTTP trace layer: one span per request with `method`, `route` and `request_id`, and an `INFO`
//! event at the end with `status` and `latency` (milliseconds).
//!
//! **What is never recorded:** the query string (OAuth `code` and `state`, search terms travel there),
//! headers (cookies, `Authorization`, keys) and bodies. `route` is the route template of axum
//! (`/api/orders/{id}`), so ids in paths do not end up in the logs either. A request that matched no
//! route (e.g. a `404`, often a scanner or a mistyped link with a token in it) records the placeholder
//! [`UNMATCHED`]; [`RouteSpan::log_unmatched_path`] opts in to a truncated raw path, still without the
//! query.
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

/// `route` of a request that matched no route.
pub const UNMATCHED: &str = "<unmatched>";

/// The trace layer type of [`layer`].
pub type HttpTraceLayer = TraceLayer<SharedClassifier<ServerErrorsAsFailures>, RouteSpan>;

/// [`MakeSpan`] recording method, route template and request id.
#[derive(Debug, Clone, Copy, Default)]
pub struct RouteSpan {
    /// `Some(n)`: unmatched requests record at most `n` bytes of their raw path.
    unmatched_path: Option<usize>,
}

impl RouteSpan {
    /// Unmatched requests record [`UNMATCHED`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Unmatched requests record their raw path (without query), cut to at most `max_len` bytes.
    /// Opt-in: paths of unknown routes may carry tokens or personal data.
    pub fn log_unmatched_path(mut self, max_len: usize) -> Self {
        self.unmatched_path = Some(max_len);
        self
    }

    /// The `route` field for `req`.
    fn route<'a, B>(&self, req: &'a Request<B>) -> &'a str {
        if let Some(m) = req.extensions().get::<MatchedPath>() {
            return m.as_str();
        }
        let Some(max) = self.unmatched_path else {
            return UNMATCHED;
        };
        let path = req.uri().path();
        // The path is ASCII (hyper rejects anything else), so every index is a char boundary;
        // `get` keeps it panic-free anyway.
        path.get(..max.min(path.len())).unwrap_or(UNMATCHED)
    }
}

impl<B> MakeSpan<B> for RouteSpan {
    /// The `request` span.
    fn make_span(&mut self, req: &Request<B>) -> Span {
        let route = self.route(req);
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

/// The trace layer with [`RouteSpan::new`]; see [`layer_with`].
pub fn layer() -> HttpTraceLayer {
    layer_with(RouteSpan::new())
}

/// The trace layer: `span`, the end of each request at `INFO` with status and latency in ms, `5xx` and
/// failures at `ERROR` (tower-http's defaults, without headers).
pub fn layer_with(span: RouteSpan) -> HttpTraceLayer {
    TraceLayer::new_for_http().make_span_with(span).on_response(
        DefaultOnResponse::new()
            .level(Level::INFO)
            .latency_unit(LatencyUnit::Millis),
    )
}
