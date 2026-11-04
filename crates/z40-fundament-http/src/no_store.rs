//! `Cache-Control: no-store` (and `Pragma: no-cache` for old caches) on API responses: they depend on a
//! session or a key and may carry personal data, so neither the browser nor a proxy may keep them. One
//! layer, so no route forgets it.
//!
//! ```
//! use axum::{Router, routing::get};
//! use z40_fundament_http::no_store::NoStore;
//!
//! let app: Router = Router::new()
//!     .route("/api/me", get(|| async { "me" }))
//!     .route("/api/info", get(|| async { "public" }))
//!     .layer(NoStore::new().prefix("/api/").except_path("/api/info"));
//! ```

use crate::middleware::{Hook, Sealed, hook_layer};
use crate::paths::Scope;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use std::sync::Arc;

/// Layer setting `Cache-Control: no-store` and `Pragma: no-cache`. Without path rules it applies to
/// every response passing through it; with [`NoStore::prefix`] / [`NoStore::path`] only to those paths.
/// The headers replace values set by a handler – a route that may be cached is listed in
/// [`NoStore::except_path`] / [`NoStore::except_prefix`] instead.
#[derive(Debug, Clone, Default)]
pub struct NoStore {
    scope: Arc<Scope>,
}

impl NoStore {
    /// Applies to every response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies to paths below `prefix` (e.g. `/api/`; matched at segment boundaries, so `/auth` covers
    /// `/auth/logout`, not `/authority`). The first path rule turns "every response" into "only these".
    pub fn prefix(mut self, prefix: &str) -> Self {
        Arc::make_mut(&mut self.scope).only.add_prefix(prefix);
        self
    }

    /// Also applies to the exact path `path`.
    pub fn path(mut self, path: &str) -> Self {
        Arc::make_mut(&mut self.scope).only.add_path(path);
        self
    }

    /// Never applies to the exact path `path` (e.g. a public endpoint with its own `Cache-Control`).
    pub fn except_path(mut self, path: &str) -> Self {
        Arc::make_mut(&mut self.scope).except.add_path(path);
        self
    }

    /// Never applies to paths below `prefix` (segment boundaries as in [`NoStore::prefix`]).
    pub fn except_prefix(mut self, prefix: &str) -> Self {
        Arc::make_mut(&mut self.scope).except.add_prefix(prefix);
        self
    }

    /// Whether responses for `path` get the headers.
    pub fn applies(&self, path: &str) -> bool {
        self.scope.applies(path)
    }
}

/// Sets `Cache-Control: no-store` and `Pragma: no-cache` on `res`, replacing earlier values.
pub fn set_no_store(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

impl Sealed for NoStore {}

impl Hook for NoStore {
    type Carry = bool;

    /// Remembers whether the path is covered.
    fn on_request(&self, req: &mut Request) -> Result<bool, Response> {
        Ok(self.applies(req.uri().path()))
    }

    /// Sets the headers on covered paths.
    fn on_response(&self, applies: bool, res: &mut Response) {
        if applies {
            set_no_store(res);
        }
    }
}

hook_layer!(NoStore);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(NoStore::new().applies("/anything"));
        let n = NoStore::new()
            .prefix("/api/")
            .prefix("/fundament/")
            .path("/auth/logout")
            .except_path("/api/info");
        assert!(
            n.applies("/api/me") && n.applies("/fundament/v1/events") && n.applies("/auth/logout")
        );
        assert!(!n.applies("/api/info") && !n.applies("/healthz") && !n.applies("/_next/x.js"));
    }
}
