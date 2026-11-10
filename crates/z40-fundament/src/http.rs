//! What the axum endpoints of this crate (status, feed source, GraphQL subgraph) share: the error form `{"code","message"}`
//! with its fixed texts (`docs/protocol.md` §3.2), `Cache-Control: no-store`, the client address for the
//! failure brake and the bearer key check in the order brake → key. The subgraph answers in GraphQL error
//! shape but with the same statuses, texts and headers ([`Rejection`]).

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

/// Called on every request with a missing or wrong key (not on braked ones), see
/// `StatusEndpoint::on_failed_attempt` and `FeedEndpoint::on_failed_attempt`.
pub type FailedAttemptHook = Arc<dyn Fn() + Send + Sync>;

/// Message of `401`.
const MSG_UNAUTHENTICATED: &str = "Anmeldung erforderlich.";
/// Message of `429`.
const MSG_RATE_LIMITED: &str = "Zu viele Fehlversuche. Bitte warten Sie eine Minute.";
/// Message of `500`.
pub(crate) const MSG_INTERNAL: &str = "Interner Fehler. Bitte versuchen Sie es später erneut.";

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
#[cfg_attr(
    not(any(feature = "status", feature = "feed-source")),
    allow(dead_code)
)]
pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    no_store((status, Json(json!({ "code": code, "message": message }))).into_response())
}

/// `500 INTERNAL` (details are logged by the caller, never sent).
#[cfg_attr(
    not(any(feature = "status", feature = "feed-source")),
    allow(dead_code)
)]
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
#[cfg_attr(
    not(any(feature = "status", feature = "feed-source")),
    allow(dead_code)
)]
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

    /// Brake → key, without building an answer (endpoints with another error shape build their own from
    /// the [`Rejection`]). A missing or wrong key is counted at the brake; the key is never logged, the
    /// client address only shortened.
    pub fn verify<'a>(&'a self, parts: &Parts) -> Result<&'a str, Rejection> {
        let ip = self.address(parts);
        let now = Instant::now();
        if self.brake.is_blocked(ip, now) {
            return Err(Rejection::Braked);
        }
        let caller = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token)
            .and_then(|k| self.keys.verify(k));
        if let Some(caller) = caller {
            return Ok(caller);
        }
        self.brake.record_failure(ip, now);
        let net = shortened_for_logs(ip);
        tracing::debug!(%net, endpoint = self.what, "request with missing or wrong key");
        Err(Rejection::Unauthenticated)
    }

    /// [`Gate::verify`] with the answers in the form `{"code","message"}`: `429` with `Retry-After: 60`,
    /// `401` with `WWW-Authenticate: Bearer`.
    #[cfg_attr(
        not(any(feature = "status", feature = "feed-source")),
        allow(dead_code)
    )]
    pub fn check<'a>(&'a self, parts: &Parts) -> Checked<'a> {
        match self.verify(parts) {
            Ok(caller) => Checked::Caller(caller),
            Err(r) => {
                let mut res = error(r.status(), r.code(), r.message());
                r.add_header(&mut res);
                match r {
                    Rejection::Braked => Checked::Braked(res),
                    Rejection::Unauthenticated => Checked::Unauthenticated(res),
                }
            }
        }
    }
}

/// Why [`Gate::verify`] refused a request; status, code, message and header are the same for every
/// endpoint, only the shape of the body differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// The address is braked (not counted again).
    Braked,
    /// Missing or wrong key (counted at the brake).
    Unauthenticated,
}

impl Rejection {
    /// `429` or `401`.
    pub fn status(self) -> StatusCode {
        match self {
            Self::Braked => StatusCode::TOO_MANY_REQUESTS,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
        }
    }

    /// `RATE_LIMITED` or `UNAUTHENTICATED`.
    pub fn code(self) -> &'static str {
        match self {
            Self::Braked => "RATE_LIMITED",
            Self::Unauthenticated => "UNAUTHENTICATED",
        }
    }

    /// The fixed text (`docs/protocol.md` §3.2).
    pub fn message(self) -> &'static str {
        match self {
            Self::Braked => MSG_RATE_LIMITED,
            Self::Unauthenticated => MSG_UNAUTHENTICATED,
        }
    }

    /// Sets `Retry-After: 60` or `WWW-Authenticate: Bearer` on `res`.
    pub fn add_header(self, res: &mut Response) {
        let (name, value) = match self {
            Self::Braked => (header::RETRY_AFTER, "60"),
            Self::Unauthenticated => (header::WWW_AUTHENTICATE, "Bearer"),
        };
        res.headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
}
