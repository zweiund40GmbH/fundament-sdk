//! HTTP groundwork for axum apps, the parts every app otherwise copies: request ids, a trace layer that
//! never logs secrets, the global log setup, security headers, `no-store`, the client address behind
//! trusted proxies and a CSRF header guard.
//!
//! Configuration uses the `from_lookup` helpers of [`z40_fundament::config`] (one system for both
//! crates); `docs/configuration.md` in the repository lists the variables.
//!
//! Always available (no feature):
//!
//! * [`security_headers`] – CSP (checked: no unsafe script sources, `frame-ancestors` required and not open),
//!   `X-Frame-Options`, `nosniff`, `Referrer-Policy`, `Permissions-Policy`, `Cross-Origin-*`; no HSTS
//! * [`no_store`] – `Cache-Control: no-store` for chosen paths
//! * [`client_ip`] – client address with `X-Forwarded-For` only from trusted proxies; extractor and a
//!   resolver for the endpoints of `z40-fundament`
//! * [`csrf`] – custom header required on state-changing requests, exempt paths, own rejection
//! * [`request_id`] – the header name and the rule for acceptable ids
//! * [`config`] – the configuration reference of this crate
//!
//! Behind features:
//!
//! * `request-id` → [`request_id::RequestIdLayer`] – keep a safe incoming `X-Request-Id` or create a
//!   UUID v7, echo it
//! * `trace` → [`trace`] – the trace layer (method, route template, request id, status, latency)
//! * `telemetry` → [`telemetry`] – JSON or one-line text logs from `<PREFIX>_LOG` / `<PREFIX>_LOG_FORMAT`
//!
//! The layers are made for axum's `Router::layer`. A typical order (the last `layer` call runs first):
//!
//! ```
//! # #[cfg(all(feature = "request-id", feature = "trace"))] {
//! use axum::{Router, routing::post};
//! use z40_fundament_http::{csrf::Csrf, no_store::NoStore, request_id::RequestIdLayer};
//! use z40_fundament_http::{security_headers::SecurityHeaders, trace};
//!
//! # fn main() -> Result<(), z40_fundament_http::csrf::CsrfConfigError> {
//! let app: Router = Router::new()
//!     .route("/api/orders", post(|| async { "created" }))
//!     .layer(Csrf::header("x-requested-with", "crm")?.protect_prefix("/api/"))
//!     .layer(NoStore::new().prefix("/api/"))
//!     .layer(SecurityHeaders::api())
//!     .layer(trace::layer())
//!     .layer(RequestIdLayer);
//! # Ok(())
//! # }
//! # }
//! ```
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod client_ip;
pub mod config;
pub mod csrf;
pub mod middleware;
pub mod no_store;
mod paths;
pub mod request_id;
pub mod security_headers;
#[cfg(feature = "telemetry")]
#[cfg_attr(docsrs, doc(cfg(feature = "telemetry")))]
pub mod telemetry;
#[cfg(feature = "trace")]
#[cfg_attr(docsrs, doc(cfg(feature = "trace")))]
pub mod trace;
