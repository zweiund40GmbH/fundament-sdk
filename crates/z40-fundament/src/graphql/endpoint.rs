//! `POST /fundament/v1/graphql` as an axum router (`docs/protocol.md` §4). Order of the checks: brake
//! (`429`) → subgraph key (`401`, counted) → body (`413` over the limit, `400` unreadable) → GraphQL request
//! (`400`) → bracket scan, parsing and document pre-check (`400`) → execution by the app under the deadline
//! (`200`, `503` on expiry). Other methods get `405`. Errors in GraphQL error shape with `extensions.code`,
//! at most [`SubgraphLimits::max_errors`] of the engine's per answer; every answer carries
//! `Cache-Control: no-store`.

use super::{ReadBudget, ReadLimiter, SubgraphLimits, Violation, precheck, scan_query};
use crate::credentials::{FailureBrake, KeyList};
use crate::http::{FailedAttemptHook, Gate, Rejection, json_ok, no_store};
use crate::protocol::GRAPHQL_PATH;
use async_graphql::parser::parse_query;
use async_graphql::parser::types::ExecutableDocument;
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use serde_json::json;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;

/// Message of `400` (unreadable body, no GraphQL request, rejected document) and `405`.
const MSG_INVALID: &str = "Ungültige Anfrage.";
/// Message of `503` (the request deadline passed).
const MSG_TIMEOUT: &str = "Zeitüberschreitung.";
/// Message of `413`.
const MSG_TOO_LARGE: &str = "Anfrage zu groß.";
/// Message of the note that replaces the errors beyond the error limit.
const MSG_MORE_ERRORS: &str = "Weitere Fehler ausgelassen.";

/// The name of the subgraph key a request was made with, in the request data of every request
/// (`ctx.data::<SubgraphCaller>()`), e.g. for logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubgraphCaller(String);

impl SubgraphCaller {
    /// The name of the key (the `name` of `name=sha256hex`).
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// The subgraph endpoint: the app's executor, the accepted subgraph keys, the brake, the limits and the
/// process-wide read limiter.
///
/// The executor gets the checked request – with a fresh [`ReadBudget`] and the [`SubgraphCaller`] already
/// in its request data – and the same budget (e.g. to log [`ReadBudget::reads`]); usually it just runs
/// `schema.execute(request)`. Build the schema with the same limits ([`SubgraphLimits::apply`]) and with
/// `EmptyMutation`, so the subgraph cannot change anything.
///
/// **The checked document is the executed one.** The endpoint parses the query itself and hands the
/// parsed document to the engine, which does not parse again. Do not register async-graphql extensions
/// that rewrite the query (persisted queries, `prepare_request` hooks): they would run a document the
/// pre-check never saw.
///
/// **Engine errors** without a path (validation, depth, complexity) and without `extensions.code` get
/// `extensions.code = "VALIDATION"`; field errors and errors with a code are passed on unchanged. An answer
/// carries at most [`SubgraphLimits::max_errors`] errors; the rest is replaced by one note.
///
/// **Client address and brake.** As for [`crate::status::StatusEndpoint`]: serve the app with
/// `into_make_service_with_connect_info::<SocketAddr>()`, or behind a reverse proxy pass
/// [`SubgraphEndpoint::client_ip`] a function that trusts `X-Forwarded-For` only from the app's proxies.
/// Share one brake with the status and feed endpoints ([`SubgraphEndpoint::brake`]). The subgraph keys are
/// a list of their own.
pub struct SubgraphEndpoint<E> {
    execute: E,
    gate: Gate,
    limits: SubgraphLimits,
    limiter: ReadLimiter,
    limiter_set: bool,
    on_failed_attempt: Option<FailedAttemptHook>,
}

impl<E, F> SubgraphEndpoint<E>
where
    E: Fn(async_graphql::Request, ReadBudget) -> F + Send + Sync + 'static,
    F: Future<Output = async_graphql::Response> + Send,
{
    /// An endpoint running `execute` for callers presenting a key of `keys`, with the default limits.
    pub fn new(keys: KeyList, execute: E) -> Self {
        let limits = SubgraphLimits::default();
        Self {
            execute,
            gate: Gate::new(keys, "graphql"),
            limiter: ReadLimiter::from_limits(&limits),
            limits,
            limiter_set: false,
            on_failed_attempt: None,
        }
    }

    /// The limits (default [`SubgraphLimits::default`]). Unless [`SubgraphEndpoint::limiter`] set one, the
    /// read limiter is rebuilt with [`SubgraphLimits::max_concurrent_reads`] permits.
    pub fn limits(mut self, limits: SubgraphLimits) -> Self {
        self.limits = limits;
        if !self.limiter_set {
            self.limiter = ReadLimiter::from_limits(&limits);
        }
        self
    }

    /// Shares `limiter` (e.g. with a second subgraph schema of the same process).
    pub fn limiter(mut self, limiter: ReadLimiter) -> Self {
        self.limiter = limiter;
        self.limiter_set = true;
        self
    }

    /// The read limiter of this endpoint.
    pub fn read_limiter(&self) -> &ReadLimiter {
        &self.limiter
    }

    /// Shares `brake` with other endpoints (the status and feed endpoints of the same app).
    pub fn brake(mut self, brake: Arc<FailureBrake>) -> Self {
        self.gate.brake = brake;
        self
    }

    /// How the client address is resolved (default: `ConnectInfo<SocketAddr>`).
    pub fn client_ip(
        mut self,
        client_ip: impl Fn(&Parts) -> Option<IpAddr> + Send + Sync + 'static,
    ) -> Self {
        self.gate.client_ip = Arc::new(client_ip);
        self
    }

    /// Calls `hook` on every request with a missing or wrong key (not on braked ones). It runs inside the
    /// request: keep it cheap and non-blocking.
    pub fn on_failed_attempt(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_failed_attempt = Some(Arc::new(hook));
        self
    }

    /// A router with the single route `POST /fundament/v1/graphql`, to merge into the app's router. Other
    /// methods on the path get `405` with `Allow: POST`.
    pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
        let ep = Arc::new(self);
        Router::new().route(
            GRAPHQL_PATH,
            post(move |req: Request| {
                let ep = Arc::clone(&ep);
                async move { ep.handle(req).await }
            })
            .fallback(|| async { method_not_allowed() }),
        )
    }

    /// Answers one request (the router calls this; public for custom routing – route only `POST` here).
    pub async fn handle(&self, req: Request) -> Response {
        let (parts, body) = req.into_parts();
        let caller = match self.gate.verify(&parts) {
            Ok(caller) => caller,
            Err(r) => return self.rejected(r),
        };
        let bytes = match Limited::new(body, self.limits.max_body()).collect().await {
            Ok(b) => b.to_bytes(),
            Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => {
                return graphql_error(StatusCode::PAYLOAD_TOO_LARGE, "VALIDATION", MSG_TOO_LARGE);
            }
            Err(e) => {
                tracing::debug!(error = %e, caller, "subgraph body unreadable");
                return graphql_error(StatusCode::BAD_REQUEST, "BAD_REQUEST", MSG_INVALID);
            }
        };
        let Ok(mut request) = serde_json::from_slice::<async_graphql::Request>(&bytes) else {
            return graphql_error(StatusCode::BAD_REQUEST, "BAD_REQUEST", MSG_INVALID);
        };
        match checked_document(&request, &self.limits) {
            Ok(doc) => request.set_parsed_query(doc),
            Err(v) => {
                match v {
                    Some(v) => {
                        tracing::debug!(caller, violation = %v, "subgraph document rejected")
                    }
                    None => tracing::debug!(caller, "subgraph query does not parse"),
                }
                return graphql_error(StatusCode::BAD_REQUEST, "VALIDATION", MSG_INVALID);
            }
        }
        let budget = ReadBudget::from_limits(&self.limits, self.limiter.clone());
        let request = request
            .data(budget.clone())
            .data(SubgraphCaller(caller.to_owned()));
        let run = (self.execute)(request, budget.clone());
        // On expiry the future is dropped: running reads stop and give their permits back.
        let Ok(mut response) = tokio::time::timeout(self.limits.max_duration(), run).await else {
            tracing::warn!(caller, reads = budget.reads(), "subgraph request timed out");
            return graphql_error(StatusCode::SERVICE_UNAVAILABLE, "TIMEOUT", MSG_TIMEOUT);
        };
        tag_engine_errors(&mut response);
        limit_errors(&mut response, self.limits.max_errors());
        tracing::debug!(caller, reads = budget.reads(), "subgraph request");
        json_ok(response)
    }

    /// The `429`/`401` answer in GraphQL shape; a missing or wrong key also calls the hook.
    fn rejected(&self, r: Rejection) -> Response {
        if r == Rejection::Unauthenticated
            && let Some(hook) = &self.on_failed_attempt
        {
            hook();
        }
        let mut res = graphql_error(r.status(), r.code(), r.message());
        r.add_header(&mut res);
        res
    }
}

/// The parsed document of `request` if it passes the bracket scan, parses and passes the pre-check; else
/// the broken limit (`None`: it does not parse). The scan keeps the parser's recursion shallow; parsing
/// here – failing closed – and handing the document on means the engine runs exactly what was checked.
fn checked_document(
    request: &async_graphql::Request,
    limits: &SubgraphLimits,
) -> Result<ExecutableDocument, Option<Violation>> {
    scan_query(&request.query, limits).map_err(Some)?;
    let doc = parse_query(&request.query).map_err(|_| None)?;
    precheck(&doc, &request.variables, limits).map_err(Some)?;
    Ok(doc)
}

/// Gives the engine's own errors (those without a path) `extensions.code = "VALIDATION"` unless they
/// carry a code already; field errors (with a path) are the app's and stay as they are.
fn tag_engine_errors(response: &mut async_graphql::Response) {
    for e in response.errors.iter_mut().filter(|e| e.path.is_empty()) {
        let ext = e.extensions.get_or_insert_with(Default::default);
        if ext.get("code").is_none() {
            ext.set("code", "VALIDATION");
        }
    }
}

/// Keeps the first `max` errors of `response`; if there were more, adds one note with
/// `extensions.code = "VALIDATION"` instead of the rest. This bounds the answer, not the engine's work –
/// the pre-check does that.
fn limit_errors(response: &mut async_graphql::Response, max: usize) {
    if response.errors.len() <= max {
        return;
    }
    response.errors.truncate(max);
    let mut note = async_graphql::ServerError::new(MSG_MORE_ERRORS, None);
    note.extensions
        .get_or_insert_with(Default::default)
        .set("code", "VALIDATION");
    response.errors.push(note);
}

/// `405` for every method but `POST`, with `Allow: POST`.
fn method_not_allowed() -> Response {
    let mut res = graphql_error(StatusCode::METHOD_NOT_ALLOWED, "BAD_REQUEST", MSG_INVALID);
    res.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    res
}

/// An error answer `{"errors":[{"message","extensions":{"code"}}]}` with `Cache-Control: no-store`.
fn graphql_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({ "errors": [{ "message": message, "extensions": { "code": code } }] });
    no_store((status, Json(body)).into_response())
}

#[cfg(test)]
#[path = "endpoint_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "endpoint_guard_tests.rs"]
mod guard_tests;
