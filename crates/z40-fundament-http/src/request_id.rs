//! Request ids: an incoming `X-Request-Id` (from a proxy or a calling app) is kept when it is safe to log
//! – 1 to 64 characters of `[A-Za-z0-9._:-]` – and otherwise replaced by a new UUID v7. The id is set on
//! the request (header and [`RequestId`] extension), echoed on the response and recorded in the span of
//! the trace layer (feature `trace`). Add this layer **outside** the trace layer (after it in
//! `Router::layer` calls).
//!
//! The rules ([`HEADER`], [`is_acceptable`]) are always available; the layer and [`RequestId`] need the
//! feature `request-id`.
//!
//! ```
//! # #[cfg(feature = "request-id")] {
//! use axum::{Extension, Router, routing::get};
//! use z40_fundament_http::request_id::{RequestId, RequestIdLayer};
//!
//! let app: Router = Router::new()
//!     .route("/api/me", get(|Extension(id): Extension<RequestId>| async move { id.as_str().to_owned() }))
//!     .layer(RequestIdLayer);
//! # }
//! ```

#[cfg(feature = "request-id")]
mod layer;

#[cfg(feature = "request-id")]
#[cfg_attr(docsrs, doc(cfg(feature = "request-id")))]
pub use layer::{RequestId, RequestIdLayer};

/// Header name (lower case).
pub const HEADER: &str = "x-request-id";

/// Longest accepted incoming id.
pub const MAX_LEN: usize = 64;

/// Whether an incoming id is safe to log and store (`[A-Za-z0-9._:-]`, 1 to [`MAX_LEN`] characters).
pub fn is_acceptable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_sane_ids() {
        assert!(is_acceptable("0192f6e2-7c1a-7b3e-9a1d-3f2b5c6d7e8f"));
        assert!(is_acceptable("proxy:abc.123_x"));
        assert!(is_acceptable(&"a".repeat(64)));
        assert!(!is_acceptable(""));
        assert!(!is_acceptable(&"a".repeat(65)));
        assert!(!is_acceptable("abc def") && !is_acceptable("abc\"}{") && !is_acceptable("ä"));
    }
}
