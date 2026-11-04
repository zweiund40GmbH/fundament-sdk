//! The request id layer and the [`RequestId`] extension (feature `request-id`).

use super::{HEADER, is_acceptable};
use crate::middleware::{Hook, Sealed, hook_layer};
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::response::Response;

/// The id of the current request (request extension).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(HeaderValue);

impl RequestId {
    /// A new time-ordered UUID v7.
    pub fn generate() -> Self {
        // A hyphenated UUID is always a valid header value; the fallback is never taken.
        let id = uuid::Uuid::now_v7().hyphenated().to_string();
        Self(HeaderValue::from_str(&id).unwrap_or(HeaderValue::from_static("-")))
    }

    /// The incoming value if [`is_acceptable`], otherwise a new id.
    pub fn accept_or_generate(incoming: Option<&HeaderValue>) -> Self {
        match incoming {
            Some(v) if v.to_str().is_ok_and(is_acceptable) => Self(v.clone()),
            _ => Self::generate(),
        }
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        // Only acceptable (ASCII) or generated values are stored.
        self.0.to_str().unwrap_or_default()
    }

    /// The id as header value.
    pub fn header_value(&self) -> &HeaderValue {
        &self.0
    }
}

/// The layer: keeps or replaces the incoming id, sets it on request and response.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestIdLayer;

impl Sealed for RequestIdLayer {}

impl Hook for RequestIdLayer {
    type Carry = HeaderValue;

    /// Sets the checked or new id on the request.
    fn on_request(&self, req: &mut Request) -> Result<HeaderValue, Response> {
        let name = HeaderName::from_static(HEADER);
        let id = RequestId::accept_or_generate(req.headers().get(&name));
        let value = id.header_value().clone();
        req.headers_mut().insert(name, value.clone());
        req.extensions_mut().insert(id);
        Ok(value)
    }

    /// Echoes the id unless the handler set one.
    fn on_response(&self, id: HeaderValue, res: &mut Response) {
        res.headers_mut()
            .entry(HeaderName::from_static(HEADER))
            .or_insert(id);
    }
}

hook_layer!(RequestIdLayer);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_uuid_v7() {
        let id = RequestId::generate();
        let parsed = uuid::Uuid::parse_str(id.as_str()).unwrap();
        assert_eq!(parsed.get_version_num(), 7);
        assert_ne!(RequestId::generate(), id);
        let bad = HeaderValue::from_static("x y");
        assert_ne!(RequestId::accept_or_generate(Some(&bad)).as_str(), "x y");
        let good = HeaderValue::from_static("abc");
        assert_eq!(RequestId::accept_or_generate(Some(&good)).as_str(), "abc");
    }
}
