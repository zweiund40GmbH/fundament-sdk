//! What the axum endpoints of this crate (status, feed source) share: the error form `{"code","message"}`
//! with its fixed texts (`docs/protocol.md` §3.2), `Cache-Control: no-store`, the client address for the
//! failure brake and the bearer key check in the order brake → key.

use crate::credentials::{FailureBrake, KeyList, bearer_token, shortened_for_logs};
use axum::Json;
use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Resolves the client address of a request for the brake. The default reads axum's
/// `ConnectInfo<SocketAddr>`; behind a reverse proxy pass a function (it may capture the app's proxy
/// configuration) that takes `X-Forwarded-For` only from trusted proxy addresses.
pub type ClientIp = Arc<dyn Fn(&Parts) -> Option<IpAddr> + Send + Sync>;

/// Message of `401`.
const MSG_UNAUTHENTICATED: &str = "Anmeldung erforderlich.";
/// Message of `429`.
const MSG_RATE_LIMITED: &str = "Zu viele Fehlversuche. Bitte warten Sie eine Minute.";
/// Message of `500`.
const MSG_INTERNAL: &str = "Interner Fehler. Bitte versuchen Sie es später erneut.";

/// The default [`ClientIp`]: the address from `ConnectInfo<SocketAddr>` (set by
/// `into_make_service_with_connect_info`).
pub(crate) fn connect_info() -> ClientIp {
    Arc::new(|parts: &Parts| {
        parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
    })
}

/// Sets `Cache-Control: no-store`.
pub(crate) fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// `200` with `body` as JSON and `Cache-Control: no-store`.
pub(crate) fn json_ok<T: serde::Serialize>(body: T) -> Response {
    no_store(Json(body).into_response())
}

/// An error answer `{"code","message"}`.
pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    no_store((status, Json(json!({ "code": code, "message": message }))).into_response())
}

/// `500 INTERNAL` (details are logged by the caller, never sent).
pub(crate) fn internal() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", MSG_INTERNAL)
}

/// The key check of an endpoint: accepted keys, the failure brake and the client address.
pub(crate) struct Gate {
    /// Accepted keys.
    pub keys: KeyList,
    /// Failed attempts per address.
    pub brake: Arc<FailureBrake>,
    /// Client address resolution.
    pub client_ip: ClientIp,
    /// What the endpoint is called in logs, e.g. `status`.
    pub what: &'static str,
    /// Whether the warning about unknown client addresses was logged.
    warned_unknown_ip: AtomicBool,
}

/// Result of [`Gate::check`].
pub(crate) enum Checked<'a> {
    /// The name of the accepted key.
    Caller(&'a str),
    /// The address is braked: the `429` answer (not counted again).
    Braked(Response),
    /// Missing or wrong key: the `401` answer (counted at the brake).
    Unauthenticated(Response),
}

impl Gate {
    /// A gate for `keys` with a fresh brake and the default address resolution.
    pub fn new(keys: KeyList, what: &'static str) -> Self {
        Self {
            keys,
            brake: Arc::new(FailureBrake::new()),
            client_ip: connect_info(),
            what,
            warned_unknown_ip: AtomicBool::new(false),
        }
    }

    /// The client address of `parts`, or `0.0.0.0` (with a one-time warning) when it is unknown.
    fn address(&self, parts: &Parts) -> IpAddr {
        if let Some(ip) = (self.client_ip)(parts) {
            return ip;
        }
        if !self.warned_unknown_ip.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                endpoint = self.what,
                "endpoint cannot see client addresses: all clients share one failure brake; \
                 serve with into_make_service_with_connect_info or pass client_ip"
            );
        }
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    }

    /// Brake (`429` with `Retry-After: 60`) → key (`401` with `WWW-Authenticate: Bearer`, counted). The
    /// key is never logged; the client address only shortened.
    pub fn check<'a>(&'a self, parts: &Parts) -> Checked<'a> {
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
            return Checked::Braked(res);
        }
        let caller = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token)
            .and_then(|k| self.keys.verify(k));
        if let Some(caller) = caller {
            return Checked::Caller(caller);
        }
        self.brake.record_failure(ip, now);
        let net = shortened_for_logs(ip);
        tracing::debug!(%net, endpoint = self.what, "request with missing or wrong key");
        let mut res = error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            MSG_UNAUTHENTICATED,
        );
        res.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        Checked::Unauthenticated(res)
    }
}
