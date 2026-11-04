//! `GET /fundament/v1/status` as an axum router. Order of the checks: brake (`429`) → status key
//! (`401`) → document (`200`, or `500` after the deadline). Every answer carries
//! `Cache-Control: no-store`; errors have the form `{"code","message"}` (`docs/protocol.md` §3.1).

use super::document::StatusDocument;
use crate::credentials::{FailureBrake, KeyList};
use crate::http::{Checked, Gate, internal, json_ok};
use crate::protocol::STATUS_PATH;
use axum::Router;
use axum::extract::Request;
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::get;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

pub use crate::http::ClientIp;

/// Error of a [`StatusProvider`] (logged, never sent to the client).
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Default deadline for building the document (the protocol allows 2 s for the whole answer).
pub const DEFAULT_DEADLINE: Duration = Duration::from_millis(1500);

/// What the app implements: the document for an authenticated caller.
pub trait StatusProvider: Send + Sync + 'static {
    /// Loads the figures and builds the document (usually with [`super::StatusBuilder`]). `caller` is
    /// the name of the status key, e.g. for logs.
    fn status(&self, caller: &str)
    -> impl Future<Output = Result<StatusDocument, BoxError>> + Send;
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
    gate: Gate,
    deadline: Duration,
    on_failed_attempt: Option<FailedAttemptHook>,
}

/// Called on every status request with a missing or wrong key (see
/// [`StatusEndpoint::on_failed_attempt`]).
pub type FailedAttemptHook = Arc<dyn Fn() + Send + Sync>;

impl<P: StatusProvider> StatusEndpoint<P> {
    /// An endpoint answering with `provider` for callers presenting a key of `keys`.
    pub fn new(provider: P, keys: KeyList) -> Self {
        Self {
            provider,
            gate: Gate::new(keys, "status"),
            deadline: DEFAULT_DEADLINE,
            on_failed_attempt: None,
        }
    }

    /// Calls `hook` on every request with a missing or wrong status key (not on braked ones), e.g. to
    /// count failed status attempts for a note of the document. It runs inside the request: keep it
    /// cheap and non-blocking (e.g. increment an atomic counter that a worker stores).
    pub fn on_failed_attempt(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_failed_attempt = Some(Arc::new(hook));
        self
    }

    /// Shares `brake` with other endpoints (e.g. the feed of the same app).
    pub fn brake(mut self, brake: Arc<FailureBrake>) -> Self {
        self.gate.brake = brake;
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
        self.gate.client_ip = Arc::new(client_ip);
        self
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
        let caller = match self.gate.check(parts) {
            Checked::Caller(c) => c,
            Checked::Braked(res) => return res,
            Checked::Unauthenticated(res) => {
                if let Some(hook) = &self.on_failed_attempt {
                    hook();
                }
                return res;
            }
        };
        match tokio::time::timeout(self.deadline, self.provider.status(caller)).await {
            Ok(Ok(doc)) => json_ok(doc),
            Ok(Err(e)) => {
                tracing::error!(error = %e, caller, "status could not be built");
                internal()
            }
            Err(_) => {
                tracing::error!(caller, "status took too long");
                internal()
            }
        }
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
