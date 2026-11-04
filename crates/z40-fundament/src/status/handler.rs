//! `GET /fundament/v1/status` as an axum router. Order of the checks: brake (`429`) → status key
//! (`401`) → document (`200`, or `500` after the deadline). Every answer carries
//! `Cache-Control: no-store`; errors have the form `{"code","message"}` (`docs/protocol.md` §3.1).

use super::document::StatusDocument;
use crate::credentials::{FailureBrake, KeyList, bearer_token, shortened_for_logs};
use crate::protocol::STATUS_PATH;
use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Error of a [`StatusProvider`] (logged, never sent to the client).
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Resolves the client address of a request for the brake. The default reads axum's
/// `ConnectInfo<SocketAddr>`; behind a reverse proxy pass a function (it may capture the app's proxy
/// configuration) that takes `X-Forwarded-For` only from trusted proxy addresses.
pub type ClientIp = Arc<dyn Fn(&Parts) -> Option<IpAddr> + Send + Sync>;

/// Default deadline for building the document (the protocol allows 2 s for the whole answer).
pub const DEFAULT_DEADLINE: Duration = Duration::from_millis(1500);
/// Message of `401`.
const MSG_UNAUTHENTICATED: &str = "Anmeldung erforderlich.";
/// Message of `429`.
const MSG_RATE_LIMITED: &str = "Zu viele Fehlversuche. Bitte warten Sie eine Minute.";
/// Message of `500`.
const MSG_INTERNAL: &str = "Interner Fehler. Bitte versuchen Sie es später erneut.";

/// What the app implements: the document for an authenticated caller.
pub trait StatusProvider: Send + Sync + 'static {
    /// Loads the figures and builds the document (usually with [`super::StatusBuilder`]). `caller` is
    /// the name of the status key, e.g. for logs.
    fn status(&self, caller: &str)
    -> impl Future<Output = Result<StatusDocument, BoxError>> + Send;
}

/// The client address from `ConnectInfo<SocketAddr>` (set by `into_make_service_with_connect_info`).
fn connect_info_ip(parts: &Parts) -> Option<IpAddr> {
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// The status endpoint: provider, accepted keys, brake, deadline and client address resolution.
///
/// **Client address.** The brake counts failed attempts per client address, so the endpoint must know
/// it. Either serve the app with `into_make_service_with_connect_info::<SocketAddr>()` (the default
/// reads `ConnectInfo`), or – behind a reverse proxy – pass [`StatusEndpoint::client_ip`] a function
/// that trusts `X-Forwarded-For` **only** when the direct peer is one of the app's proxies (anyone else
/// could send the header and dodge the brake). Requests without a known address are counted together
/// under `0.0.0.0`; the first such request logs a warning, because then every client shares one brake.
pub struct StatusEndpoint<P> {
    provider: P,
    keys: KeyList,
    brake: Arc<FailureBrake>,
    deadline: Duration,
    client_ip: ClientIp,
    warned_unknown_ip: AtomicBool,
}

impl<P: StatusProvider> StatusEndpoint<P> {
    /// An endpoint answering with `provider` for callers presenting a key of `keys`.
    pub fn new(provider: P, keys: KeyList) -> Self {
        Self {
            provider,
            keys,
            brake: Arc::new(FailureBrake::new()),
            deadline: DEFAULT_DEADLINE,
            client_ip: Arc::new(connect_info_ip),
            warned_unknown_ip: AtomicBool::new(false),
        }
    }

    /// Shares `brake` with other endpoints (e.g. the feed of the same app).
    pub fn brake(mut self, brake: Arc<FailureBrake>) -> Self {
        self.brake = brake;
        self
    }

    /// Deadline for the provider (default [`DEFAULT_DEADLINE`]).
    pub fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// How the client address is resolved (default: `ConnectInfo<SocketAddr>`). See the type docs for
    /// the reverse-proxy case.
    pub fn client_ip(
        mut self,
        client_ip: impl Fn(&Parts) -> Option<IpAddr> + Send + Sync + 'static,
    ) -> Self {
        self.client_ip = Arc::new(client_ip);
        self
    }

    /// The client address of `parts`, or `0.0.0.0` (with a one-time warning) when it is unknown.
    fn address(&self, parts: &Parts) -> IpAddr {
        if let Some(ip) = (self.client_ip)(parts) {
            return ip;
        }
        if !self.warned_unknown_ip.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "status endpoint cannot see client addresses: all clients share one failure brake; \
                 serve with into_make_service_with_connect_info or pass StatusEndpoint::client_ip"
            );
        }
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    }

    /// A router with the single route `GET /fundament/v1/status`, to merge into the app's router.
    pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
        let ep = Arc::new(self);
        Router::new().route(
            STATUS_PATH,
            get(move |req: Request| {
                let ep = Arc::clone(&ep);
                async move {
                    let (parts, _body) = req.into_parts();
                    ep.handle(&parts).await
                }
            }),
        )
    }

    /// Answers one request (the router calls this; public for custom routing).
    pub async fn handle(&self, parts: &Parts) -> Response {
        let ip = self.address(parts);
        let now = Instant::now();
        if self.brake.is_blocked(ip, now) {
            let mut res = error(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                MSG_RATE_LIMITED,
            );
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
            return res;
        }
        let caller = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token)
            .and_then(|k| self.keys.verify(k));
        let Some(caller) = caller else {
            self.brake.record_failure(ip, now);
            let net = shortened_for_logs(ip);
            tracing::debug!(%net, "status request with missing or wrong key");
            let mut res = error(
                StatusCode::UNAUTHORIZED,
                "UNAUTHENTICATED",
                MSG_UNAUTHENTICATED,
            );
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return res;
        };
        match tokio::time::timeout(self.deadline, self.provider.status(caller)).await {
            Ok(Ok(doc)) => no_store(Json(doc).into_response()),
            Ok(Err(e)) => {
                tracing::error!(error = %e, caller, "status could not be built");
                error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", MSG_INTERNAL)
            }
            Err(_) => {
                tracing::error!(caller, "status took too long");
                error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", MSG_INTERNAL)
            }
        }
    }
}

/// Sets `Cache-Control: no-store`.
fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// An error answer `{"code","message"}`.
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    no_store((status, Json(json!({ "code": code, "message": message }))).into_response())
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
