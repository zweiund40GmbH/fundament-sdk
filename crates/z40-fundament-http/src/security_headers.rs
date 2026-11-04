//! Security headers on every response: `Content-Security-Policy` (with `frame-ancestors`, never
//! `'unsafe-inline'` for scripts), `X-Frame-Options` for old browsers, `X-Content-Type-Options`,
//! `Referrer-Policy`, `Permissions-Policy` and `Cross-Origin-Opener-Policy` / `-Resource-Policy`.
//!
//! **Only missing headers are added.** A route that needs another policy (a preview page that may be
//! framed by the app itself, a development tool) sets its own `Content-Security-Policy`; the layer then
//! keeps it and derives `X-Frame-Options` from **that** policy's `frame-ancestors` (`'none'` → `DENY`,
//! `'self'` → `SAMEORIGIN`, anything else → not sent). A route's own CSP is not checked here; test it
//! with [`check_csp`]. For a different CSP per route group, add a second
//! [`SecurityHeaders`] layer to that group's router before merging it: the inner layer runs first and
//! the outer one only fills what is still missing.
//!
//! **HSTS is not set here.** `Strict-Transport-Security` belongs to the edge proxy that terminates
//! TLS (one place for every app on the host, and never sent over plain HTTP by mistake).
//!
//! ```
//! use axum::{Router, routing::get};
//! use z40_fundament_http::security_headers::{CspError, SecurityHeaders};
//!
//! # fn main() -> Result<(), CspError> {
//! let api: Router = Router::new()
//!     .route("/api/me", get(|| async { "{}" }))
//!     .layer(SecurityHeaders::api());
//! let web: Router = Router::new()
//!     .route("/", get(|| async { "<!doctype html>" }))
//!     .layer(SecurityHeaders::api().csp(
//!         "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
//!          frame-ancestors 'none'",
//!     )?);
//! let app = api.merge(web);
//! # Ok(())
//! # }
//! ```

mod csp;

pub use csp::{CspError, check_csp};

use crate::middleware::{Hook, Sealed, hook_layer};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::response::Response;
use std::sync::Arc;

/// CSP of [`SecurityHeaders::api`]: JSON answers load nothing and are never framed.
pub const API_CSP: &str =
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// Default `Permissions-Policy`: no access to sensors, camera, microphone, payment or USB.
pub const PERMISSIONS_POLICY: &str = "accelerometer=(), camera=(), geolocation=(), gyroscope=(), \
     magnetometer=(), microphone=(), payment=(), usb=()";

/// The configured headers of the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Values {
    csp: HeaderValue,
    /// `X-Frame-Options` derived from `frame-ancestors` (`None` when it has no equivalent).
    frame_options: Option<HeaderValue>,
    referrer_policy: HeaderValue,
    permissions_policy: Option<HeaderValue>,
    opener_policy: Option<HeaderValue>,
    resource_policy: Option<HeaderValue>,
}

/// The security header layer. Start with [`SecurityHeaders::api`] and adjust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityHeaders {
    values: Arc<Values>,
}

impl SecurityHeaders {
    /// Headers for an API: [`API_CSP`], `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`,
    /// [`PERMISSIONS_POLICY`], `Cross-Origin-Opener-Policy` and `Cross-Origin-Resource-Policy`
    /// `same-origin`, `X-Content-Type-Options: nosniff`.
    pub fn api() -> Self {
        Self {
            values: Arc::new(Values {
                csp: HeaderValue::from_static(API_CSP),
                frame_options: Some(HeaderValue::from_static("DENY")),
                referrer_policy: HeaderValue::from_static("no-referrer"),
                permissions_policy: Some(HeaderValue::from_static(PERMISSIONS_POLICY)),
                opener_policy: Some(HeaderValue::from_static("same-origin")),
                resource_policy: Some(HeaderValue::from_static("same-origin")),
            }),
        }
    }

    /// Replaces the CSP after [`check_csp`]; `X-Frame-Options` follows its `frame-ancestors`
    /// (`'none'` → `DENY`, `'self'` → `SAMEORIGIN`, anything else → not sent).
    pub fn csp(mut self, csp: &str) -> Result<Self, CspError> {
        let checked = csp::check(csp)?;
        let v = Arc::make_mut(&mut self.values);
        v.csp = checked.value;
        v.frame_options = checked.frame_options;
        Ok(self)
    }

    /// Replaces `Referrer-Policy` (e.g. `strict-origin-when-cross-origin` for HTML pages).
    pub fn referrer_policy(mut self, value: HeaderValue) -> Self {
        Arc::make_mut(&mut self.values).referrer_policy = value;
        self
    }

    /// Replaces `Permissions-Policy`; `None` = not sent.
    pub fn permissions_policy(mut self, value: Option<HeaderValue>) -> Self {
        Arc::make_mut(&mut self.values).permissions_policy = value;
        self
    }

    /// Replaces `Cross-Origin-Opener-Policy`; `None` = not sent.
    pub fn cross_origin_opener_policy(mut self, value: Option<HeaderValue>) -> Self {
        Arc::make_mut(&mut self.values).opener_policy = value;
        self
    }

    /// Replaces `Cross-Origin-Resource-Policy`; `None` = not sent (e.g. assets other sites may embed).
    pub fn cross_origin_resource_policy(mut self, value: Option<HeaderValue>) -> Self {
        Arc::make_mut(&mut self.values).resource_policy = value;
        self
    }

    /// Adds the missing headers to `h` (see the module docs for the rules).
    pub fn apply(&self, h: &mut HeaderMap) {
        let v = &*self.values;
        let optional: [(HeaderName, &Option<HeaderValue>); 3] = [
            (
                HeaderName::from_static("permissions-policy"),
                &v.permissions_policy,
            ),
            (
                HeaderName::from_static("cross-origin-opener-policy"),
                &v.opener_policy,
            ),
            (
                HeaderName::from_static("cross-origin-resource-policy"),
                &v.resource_policy,
            ),
        ];
        h.entry(header::X_CONTENT_TYPE_OPTIONS)
            .or_insert(HeaderValue::from_static("nosniff"));
        h.entry(header::REFERRER_POLICY)
            .or_insert_with(|| v.referrer_policy.clone());
        for (name, value) in optional {
            if let Some(value) = value {
                h.entry(name).or_insert_with(|| value.clone());
            }
        }
        let frame_options = match h.get(header::CONTENT_SECURITY_POLICY) {
            Some(own) => own.to_str().ok().and_then(csp::frame_options),
            None => {
                h.insert(header::CONTENT_SECURITY_POLICY, v.csp.clone());
                v.frame_options.clone()
            }
        };
        if let Some(xfo) = frame_options {
            h.entry(header::X_FRAME_OPTIONS).or_insert(xfo);
        }
    }
}

impl Sealed for SecurityHeaders {}

impl Hook for SecurityHeaders {
    type Carry = ();

    /// Nothing to do.
    fn on_request(&self, _req: &mut Request) -> Result<(), Response> {
        Ok(())
    }

    /// Adds the missing headers.
    fn on_response(&self, (): (), res: &mut Response) {
        self.apply(res.headers_mut());
    }
}

hook_layer!(SecurityHeaders);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_headers_and_an_own_csp_is_kept() {
        let s = SecurityHeaders::api();
        let mut h = HeaderMap::new();
        s.apply(&mut h);
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], API_CSP);
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(h["cross-origin-resource-policy"], "same-origin");
        assert!(!h.contains_key(header::STRICT_TRANSPORT_SECURITY));

        let mut own = HeaderMap::new();
        own.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'self'"),
        );
        s.apply(&mut own);
        assert_eq!(
            own[header::CONTENT_SECURITY_POLICY],
            "frame-ancestors 'self'"
        );
        assert_eq!(
            own[header::X_FRAME_OPTIONS],
            "SAMEORIGIN",
            "derived from the route's CSP"
        );
        let mut open = HeaderMap::new();
        open.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors https://example.com"),
        );
        s.apply(&mut open);
        assert!(!open.contains_key(header::X_FRAME_OPTIONS));
        let mut deny = HeaderMap::new();
        deny.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'; frame-ancestors 'none'"),
        );
        s.apply(&mut deny);
        assert_eq!(deny[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(own[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    }

    #[test]
    fn adjusted_headers() {
        let s = SecurityHeaders::api()
            .csp("default-src 'self'; frame-ancestors 'self'")
            .unwrap()
            .referrer_policy(HeaderValue::from_static("strict-origin-when-cross-origin"))
            .cross_origin_resource_policy(None);
        let mut h = HeaderMap::new();
        s.apply(&mut h);
        assert_eq!(h[header::X_FRAME_OPTIONS], "SAMEORIGIN");
        assert_eq!(
            h[header::REFERRER_POLICY],
            "strict-origin-when-cross-origin"
        );
        assert!(!h.contains_key("cross-origin-resource-policy"));
        let other = SecurityHeaders::api()
            .csp("default-src 'self'; frame-ancestors https://example.com")
            .unwrap();
        let mut h = HeaderMap::new();
        other.apply(&mut h);
        assert!(!h.contains_key(header::X_FRAME_OPTIONS));
    }
}
