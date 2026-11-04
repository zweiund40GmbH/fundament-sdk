//! Reading the status document of another app: `GET <origin>/fundament/v1/status` with the status key
//! of that app as bearer key, `Accept: application/json`, at most [`FETCH_DEADLINE`] for the whole request
//! (the protocol answers within 2 s), no redirects, at most [`FETCH_BODY_MAX`] bytes. Errors are fixed kinds
//! with fixed German texts; the key and the body are never logged, transport causes are.

use super::document::StatusDocument;
use crate::credentials::BearerKey;
use crate::http_client::{self, BodyError};
use crate::protocol::STATUS_PATH;
use std::time::Duration;

/// Time limit of one fetch, from connecting until the body is read (`docs/protocol.md` §3).
pub const FETCH_DEADLINE: Duration = Duration::from_secs(2);
/// Largest accepted body (100 events and some hundred pollers stay far below).
pub const FETCH_BODY_MAX: usize = 256 * 1024;

/// Why fetching a status failed. Carries no response contents and no key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StatusFetchError {
    /// Connection or TLS problem.
    #[error("app not reachable")]
    Unreachable,
    /// No complete answer within [`FETCH_DEADLINE`].
    #[error("app did not answer in time")]
    Timeout,
    /// Non-success HTTP status (including redirects; `401` = wrong key, `429` = braked, `404` = no
    /// status endpoint).
    #[error("app answered with HTTP {0}")]
    Status(u16),
    /// The body is not a valid status document, or is too large.
    #[error("invalid status document")]
    Invalid,
}

impl StatusFetchError {
    /// The fixed German text, e.g. for a monitoring view: `nicht erreichbar`, `Zeitüberschreitung`,
    /// `HTTP <status>`, `Antwort ungültig` (the same texts as for a consumed feed, `docs/protocol.md` §3.1,
    /// plus the timeout).
    pub fn text(self) -> String {
        match self {
            StatusFetchError::Unreachable => "nicht erreichbar".to_owned(),
            StatusFetchError::Timeout => "Zeitüberschreitung".to_owned(),
            StatusFetchError::Status(s) => format!("HTTP {s}"),
            StatusFetchError::Invalid => "Antwort ungültig".to_owned(),
        }
    }
}

/// Why a [`StatusClient`] or [`StatusTarget`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StatusClientError {
    /// The origin is not `https://host[:port]` (or, for tests, a loopback `http://` origin).
    #[error("invalid status origin")]
    InvalidUrl,
    /// The app name is not 1–64 characters of `a-z A-Z 0-9 _ . -`.
    #[error("invalid app name")]
    InvalidApp,
    /// The HTTP client could not be built (TLS backend).
    #[error("HTTP client could not be built")]
    Client,
}

/// An app to poll: its name (the key of [`super::StatusKeys`]) and its validated origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusTarget {
    app: String,
    origin: String,
}

impl StatusTarget {
    /// The target `app` at the `https://` `origin` (one trailing `/` allowed).
    pub fn new(app: &str, origin: &str) -> Result<Self, StatusClientError> {
        let origin = http_client::https_origin(origin).ok_or(StatusClientError::InvalidUrl)?;
        if !crate::credentials::name_is_valid(app) {
            return Err(StatusClientError::InvalidApp);
        }
        Ok(Self {
            app: app.to_owned(),
            origin,
        })
    }

    /// A plain-http loopback target for tests of this crate.
    #[cfg(all(test, feature = "status"))]
    pub(crate) fn loopback(app: &str, origin: &str) -> Self {
        let origin = http_client::loopback_http_origin(origin).unwrap_or_default();
        Self {
            app: app.to_owned(),
            origin,
        }
    }

    /// The app name.
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The origin, without trailing slash.
    pub fn origin(&self) -> &str {
        &self.origin
    }
}

/// Client for the status endpoint of one app. Cheap to clone (the connection pool is shared).
#[derive(Clone)]
pub struct StatusClient {
    http: reqwest::Client,
    url: String,
    key: BearerKey,
}

impl std::fmt::Debug for StatusClient {
    /// URL only; the key is redacted.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusClient")
            .field("url", &self.url)
            .field("key", &self.key)
            .finish()
    }
}

/// The HTTP client of status fetches.
pub(crate) fn http(https_only: bool) -> Result<reqwest::Client, StatusClientError> {
    http_client::build(FETCH_DEADLINE, FETCH_DEADLINE, https_only)
        .map_err(|_| StatusClientError::Client)
}

impl StatusClient {
    /// Client for the `https://` `origin` (one trailing `/` allowed), sending `key`.
    pub fn new(origin: &str, key: BearerKey) -> Result<Self, StatusClientError> {
        let origin = http_client::https_origin(origin).ok_or(StatusClientError::InvalidUrl)?;
        Ok(Self::with_http(http(true)?, &origin, key))
    }

    /// Like [`StatusClient::new`], but for a plain `http://` origin on a **loopback** address (a local
    /// server in tests).
    pub fn plain_http_for_tests(origin: &str, key: BearerKey) -> Result<Self, StatusClientError> {
        let origin =
            http_client::loopback_http_origin(origin).ok_or(StatusClientError::InvalidUrl)?;
        Ok(Self::with_http(http(false)?, &origin, key))
    }

    /// Client on a shared `http` for the validated `origin`.
    pub(crate) fn with_http(http: reqwest::Client, origin: &str, key: BearerKey) -> Self {
        Self {
            http,
            url: format!("{origin}{STATUS_PATH}"),
            key,
        }
    }

    /// Fetches and checks the status document ([`StatusDocument::from_json`]).
    pub async fn fetch(&self) -> Result<StatusDocument, StatusFetchError> {
        let res = self
            .http
            .get(&self.url)
            .bearer_auth(self.key.expose())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        if !res.status().is_success() {
            return Err(StatusFetchError::Status(res.status().as_u16()));
        }
        let body = http_client::read_body(res, FETCH_BODY_MAX)
            .await
            .map_err(|e| match e {
                BodyError::TooLarge => self.invalid("too large"),
                BodyError::Transport(e) => self.transport(e),
            })?;
        StatusDocument::from_json(&body).map_err(|e| self.invalid(e.0))
    }

    /// Maps a transport error to `Timeout` or `Unreachable` and logs its cause chain.
    fn transport(&self, e: reqwest::Error) -> StatusFetchError {
        let cause = http_client::cause_chain(&e);
        if e.is_timeout() {
            tracing::warn!(url = %self.url, %cause, "status: no answer in time");
            StatusFetchError::Timeout
        } else {
            tracing::warn!(url = %self.url, %cause, "status: app not reachable");
            StatusFetchError::Unreachable
        }
    }

    /// Logs why the answer was refused (a field name, never the body).
    fn invalid(&self, reason: &str) -> StatusFetchError {
        tracing::warn!(url = %self.url, reason, "status: invalid answer");
        StatusFetchError::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_texts() {
        assert_eq!(StatusFetchError::Unreachable.text(), "nicht erreichbar");
        assert_eq!(StatusFetchError::Timeout.text(), "Zeitüberschreitung");
        assert_eq!(StatusFetchError::Status(401).text(), "HTTP 401");
        assert_eq!(StatusFetchError::Invalid.text(), "Antwort ungültig");
    }

    #[test]
    fn origins_and_redaction() {
        let key = || BearerKey::new("s3cr3t").unwrap();
        let c = StatusClient::new("https://crm.example.com/", key()).unwrap();
        let d = format!("{c:?}");
        assert!(d.contains("https://crm.example.com/fundament/v1/status"));
        assert!(!d.contains("s3cr3t"));
        for bad in ["http://crm.example.com", "https://crm.example.com/x", "crm"] {
            assert_eq!(
                StatusClient::new(bad, key()).unwrap_err(),
                StatusClientError::InvalidUrl
            );
        }
        assert!(StatusClient::plain_http_for_tests("http://127.0.0.1:1", key()).is_ok());
        assert!(StatusClient::plain_http_for_tests("http://crm.example.com", key()).is_err());
    }

    #[test]
    fn targets() {
        let t = StatusTarget::new("crm", "https://crm.example.com:8443/").unwrap();
        assert_eq!(
            (t.app(), t.origin()),
            ("crm", "https://crm.example.com:8443")
        );
        assert_eq!(
            StatusTarget::new("a b", "https://crm.example.com"),
            Err(StatusClientError::InvalidApp)
        );
        assert_eq!(
            StatusTarget::new("crm", "http://crm.example.com"),
            Err(StatusClientError::InvalidUrl)
        );
    }
}
