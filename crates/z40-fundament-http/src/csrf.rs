//! CSRF guard: every state-changing request (any method but `GET`, `HEAD`, `OPTIONS`) must carry a
//! custom header with an app-specific value, e.g. `X-Requested-With: crm`. A cross-site form cannot set
//! a custom header, and a cross-site `fetch` with it needs a CORS preflight, which the app never grants.
//! Missing or wrong → `403` before the handler runs.
//!
//! Server-to-server endpoints without cookies (webhooks, back-channel logout, `/fundament/…` with
//! bearer keys) are exempt via [`Csrf::exempt_path`] / [`Csrf::exempt_prefix`]. An app with a
//! different check (e.g. a double-submit cookie) passes it to [`Csrf::check`].
//!
//! The default answer is `403` with the error form of the endpoints of `z40-fundament`
//! (`{"code","message"}`, German fixed text, `Cache-Control: no-store`):
//! `{"code":"CSRF","message":"Anfrage abgelehnt (CSRF-Schutz). Bitte laden Sie die Seite neu."}`.
//! Apps with their own error shape pass [`Csrf::rejection`].
//!
//! ```
//! use axum::{Router, routing::post};
//! use z40_fundament_http::csrf::{Csrf, CsrfConfigError};
//!
//! # fn main() -> Result<(), CsrfConfigError> {
//! let app: Router = Router::new()
//!     .route("/api/orders", post(|| async { "created" }))
//!     .route("/api/auth/backchannel-logout", post(|| async { "" }))
//!     .layer(
//!         Csrf::header("x-requested-with", "crm")?
//!             .exempt_path("/api/auth/backchannel-logout")
//!             .exempt_prefix("/fundament/"),
//!     );
//! # Ok(())
//! # }
//! ```

mod header;

pub use header::CsrfHeader;

use crate::middleware::{Hook, Sealed, hook_layer};
use crate::no_store::set_no_store;
use crate::paths::Scope;
use axum::Json;
use axum::extract::Request;
use axum::http::header::{HeaderName, InvalidHeaderName};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

/// `code` of the default answer.
pub const CODE: &str = "CSRF";
/// `message` of the default answer (German fixed text, like the other wire texts).
pub const MESSAGE: &str = "Anfrage abgelehnt (CSRF-Schutz). Bitte laden Sie die Seite neu.";

/// Header names a cross-site request may send without a CORS preflight (Fetch standard, safelisted
/// request headers); they prove nothing.
pub const CORS_SAFELISTED: [&str; 5] = [
    "accept",
    "accept-language",
    "content-language",
    "content-type",
    "range",
];

/// A custom check: `true` lets the request pass.
pub type CsrfCheck = Arc<dyn Fn(&HeaderMap) -> bool + Send + Sync>;
/// Builds the answer for a rejected request.
pub type CsrfRejection = Arc<dyn Fn() -> Response + Send + Sync>;

/// A header name or value that cannot be used for the guard.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CsrfConfigError {
    /// The header name is not a valid HTTP header name, or a CORS-safelisted one (`accept`,
    /// `accept-language`, `content-language`, `content-type`, `range`), which a cross-site request may
    /// send without a preflight.
    #[error("invalid CSRF header name")]
    Name,
    /// The value is empty or not a visible ASCII header value.
    #[error("invalid CSRF header value")]
    Value,
}

impl From<InvalidHeaderName> for CsrfConfigError {
    /// [`CsrfConfigError::Name`].
    fn from(_: InvalidHeaderName) -> Self {
        Self::Name
    }
}

/// The CSRF layer: check, covered paths and rejection.
#[derive(Clone)]
pub struct Csrf {
    check: CsrfCheck,
    scope: Arc<Scope>,
    rejection: CsrfRejection,
}

impl std::fmt::Debug for Csrf {
    /// Shows the path rules only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Csrf")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl Csrf {
    /// Requires header `name` with exactly `value` (byte comparison, case-sensitive value). A
    /// CORS-safelisted name ([`CORS_SAFELISTED`]) is refused.
    pub fn header(name: &str, value: &str) -> Result<Self, CsrfConfigError> {
        let name = HeaderName::try_from(name)?;
        if CORS_SAFELISTED.contains(&name.as_str()) {
            return Err(CsrfConfigError::Name);
        }
        let ok = !value.is_empty() && value.bytes().all(|b| b.is_ascii_graphic());
        let value = HeaderValue::from_str(value)
            .ok()
            .filter(|_| ok)
            .ok_or(CsrfConfigError::Value)?;
        Ok(Self::guard(name, value))
    }

    /// Requires the header of `header`, checked at compile time (see [`CsrfHeader`]): unlike
    /// [`Csrf::header`] this cannot fail, so an app needs no fallback for a start-up error.
    ///
    /// ```
    /// use z40_fundament_http::csrf::{Csrf, CsrfHeader};
    ///
    /// let layer = Csrf::header_static(const { CsrfHeader::new("x-requested-with", "crm") });
    /// ```
    pub fn header_static(header: CsrfHeader) -> Self {
        // Cannot panic: `CsrfHeader::new` accepted only lowercase tokens and visible ASCII, a subset of
        // what `from_static` takes (unit test in `header.rs`).
        let name = HeaderName::from_static(header.name);
        let value = HeaderValue::from_static(header.value);
        Self::guard(name, value)
    }

    /// The check of a header `name` with exactly `value`.
    fn guard(name: HeaderName, value: HeaderValue) -> Self {
        Self::check(move |h: &HeaderMap| {
            h.get(&name)
                .is_some_and(|v| v.as_bytes() == value.as_bytes())
        })
    }

    /// Lets a state-changing request pass when `check` returns `true` for its headers.
    pub fn check(check: impl Fn(&HeaderMap) -> bool + Send + Sync + 'static) -> Self {
        Self {
            check: Arc::new(check),
            scope: Arc::default(),
            rejection: Arc::new(default_rejection),
        }
    }

    /// Checks only paths below `prefix` (e.g. `/api/`, matched at segment boundaries: `/auth` covers
    /// `/auth/logout`, not `/authority`); without such a rule every path is checked.
    pub fn protect_prefix(mut self, prefix: &str) -> Self {
        Arc::make_mut(&mut self.scope).only.add_prefix(prefix);
        self
    }

    /// Checks the exact path `path` too (with [`Csrf::protect_prefix`]).
    pub fn protect_path(mut self, path: &str) -> Self {
        Arc::make_mut(&mut self.scope).only.add_path(path);
        self
    }

    /// Never checks the exact path `path` (a server-to-server call without cookies).
    pub fn exempt_path(mut self, path: &str) -> Self {
        Arc::make_mut(&mut self.scope).except.add_path(path);
        self
    }

    /// Never checks paths below `prefix` (e.g. `/fundament/`, which uses bearer keys; segment boundaries
    /// as in [`Csrf::protect_prefix`]).
    pub fn exempt_prefix(mut self, prefix: &str) -> Self {
        Arc::make_mut(&mut self.scope).except.add_prefix(prefix);
        self
    }

    /// Answers rejected requests with `rejection()` instead of the default `403` (the app's error shape).
    pub fn rejection(mut self, rejection: impl Fn() -> Response + Send + Sync + 'static) -> Self {
        self.rejection = Arc::new(rejection);
        self
    }

    /// Whether a request with `method` to `path` must pass the check.
    pub fn requires_check(&self, method: &Method, path: &str) -> bool {
        is_mutating(method) && self.scope.applies(path)
    }

    /// Whether a request with `method`, `path` and `headers` is allowed.
    pub fn allows(&self, method: &Method, path: &str, headers: &HeaderMap) -> bool {
        !self.requires_check(method, path) || (self.check)(headers)
    }
}

/// Whether `method` may change state (anything but `GET`, `HEAD`, `OPTIONS`).
pub fn is_mutating(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The default answer: `403 {"code":"CSRF","message":…}` with `Cache-Control: no-store`.
pub fn default_rejection() -> Response {
    let mut res = (
        StatusCode::FORBIDDEN,
        Json(json!({ "code": CODE, "message": MESSAGE })),
    )
        .into_response();
    set_no_store(&mut res);
    res
}

impl Sealed for Csrf {}

impl Hook for Csrf {
    type Carry = ();

    /// Rejects a state-changing request that fails the check.
    fn on_request(&self, req: &mut Request) -> Result<(), Response> {
        if self.allows(req.method(), req.uri().path(), req.headers()) {
            Ok(())
        } else {
            Err((self.rejection)())
        }
    }

    /// Nothing to do.
    fn on_response(&self, (): (), _res: &mut Response) {}
}

hook_layer!(Csrf);

#[cfg(test)]
mod tests;
