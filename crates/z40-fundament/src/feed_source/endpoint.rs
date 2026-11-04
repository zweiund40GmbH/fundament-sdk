//! `GET /fundament/v1/events` as an axum router. Order of the checks: brake (`429`) → feed key (`401`,
//! counted) → query (`400`) → cursor assignment → read (`200`, or `500`). Every answer carries
//! `Cache-Control: no-store`; errors have the form `{"code","message"}` (`docs/protocol.md` §2.6).

use super::failures::FailureCounter;
use super::{
    FeedQuery, FeedSourceStore, QueryError, REQUEST_ASSIGN_WAIT, assign_logged, build_page,
};
use crate::credentials::{FailureBrake, KeyList};
use crate::http::{Checked, Gate, error, internal, json_ok};
use crate::protocol::EVENTS_PATH;
use axum::Router;
use axum::extract::{Query, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::get;
use chrono::Utc;
use std::net::IpAddr;
use std::sync::Arc;

/// The feed endpoint: the store, the accepted feed keys, the brake and the client address resolution.
///
/// **Client address.** As for [`crate::status::StatusEndpoint`]: serve the app with
/// `into_make_service_with_connect_info::<SocketAddr>()`, or behind a reverse proxy pass
/// [`FeedEndpoint::client_ip`] a function that trusts `X-Forwarded-For` only from the app's proxies.
/// Share one brake with the status endpoint ([`FeedEndpoint::brake`]), as the protocol counts failed
/// attempts per address across both.
///
/// The feed keys are the `FEED_CLIENTS` list – keep them separate from the status keys.
pub struct FeedEndpoint<S> {
    store: Arc<S>,
    gate: Gate,
    failures: Arc<FailureCounter>,
}

impl<S: FeedSourceStore> FeedEndpoint<S> {
    /// An endpoint serving `store` to consumers presenting a key of `keys`.
    pub fn new(store: S, keys: KeyList) -> Self {
        Self {
            store: Arc::new(store),
            gate: Gate::new(keys, "feed"),
            failures: Arc::new(FailureCounter::default()),
        }
    }

    /// Shares `brake` with other endpoints (e.g. the status endpoint of the same app).
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

    /// A router with the single route `GET /fundament/v1/events`, to merge into the app's router.
    pub fn router<T: Clone + Send + Sync + 'static>(self) -> Router<T> {
        let ep = Arc::new(self);
        Router::new().route(
            EVENTS_PATH,
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
                // In memory; at most one task writes to the store, so a `401` never waits for the
                // database and guessers cannot create database work per request.
                self.failures.count(&self.store);
                return res;
            }
        };
        let query = match parse(parts, self.store.known_types()) {
            Ok(q) => q,
            Err(e) => {
                tracing::debug!(caller, error = %e, "feed request with invalid query");
                return error(StatusCode::BAD_REQUEST, "BAD_REQUEST", e.message());
            }
        };
        // Short wait: the worker assigns what an open transaction holds back. A failed assignment is
        // logged; the answer goes on with what is assigned.
        let _ = assign_logged(&*self.store, REQUEST_ASSIGN_WAIT).await;
        let page = match self.read(&query).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, caller, "feed could not be read");
                return internal();
            }
        };
        if let Err(e) = self
            .store
            .record_poll(caller, page.next_cursor, Utc::now())
            .await
        {
            tracing::warn!(error = %e, caller, "feed poll could not be remembered");
        }
        tracing::info!(
            caller,
            since = query.since,
            count = page.items.len(),
            "feed delivered"
        );
        json_ok(page)
    }

    /// The last assigned cursor (before reading, see [`build_page`]), then `limit + 1` events.
    async fn read(
        &self,
        q: &FeedQuery,
    ) -> Result<crate::protocol::feed::FeedPage, crate::StoreError> {
        let scanned_to = self.store.last_cursor().await?;
        let events = self
            .store
            .read(q.since, q.limit.saturating_add(1), &q.types)
            .await?;
        Ok(build_page(events, q.since, q.limit, scanned_to))
    }
}

/// The query of `parts`, checked against the known types.
fn parse(parts: &Parts, known: &[String]) -> Result<FeedQuery, QueryError> {
    let Query(params) = Query::<Vec<(String, String)>>::try_from_uri(&parts.uri)
        .map_err(|_| QueryError::Malformed)?;
    FeedQuery::parse(&params, known)
}

#[cfg(test)]
#[path = "endpoint_tests.rs"]
mod tests;
