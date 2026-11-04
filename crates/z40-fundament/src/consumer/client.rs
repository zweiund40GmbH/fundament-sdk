//! HTTP client of one feed: `GET <origin>/fundament/v1/events`. Returns the raw page; parsing is pure
//! logic in [`crate::protocol::feed`]. The key goes only into the `Authorization` header; errors carry
//! the HTTP status at most, never the response body. Redirects are not followed (the key must not
//! travel to another location); the HTTP setup is shared with the status client ([`crate::http_client`]).

use crate::credentials::BearerKey;
use crate::http_client::{self, BodyError};
use crate::protocol::EVENTS_PATH;
use std::time::Duration;

/// Largest accepted page body (500 events of master data stay far below).
pub(crate) const BODY_MAX: usize = 16 * 1024 * 1024;
/// Connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Total request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Why fetching a page failed. Carries no response contents and no key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// Connection, TLS or timeout problem.
    #[error("source not reachable")]
    Unreachable,
    /// Non-success HTTP status (including redirects).
    #[error("source answered with HTTP {0}")]
    Status(u16),
    /// The body is not a valid feed page, or is too large.
    #[error("invalid feed page")]
    Invalid,
}

impl FetchError {
    /// The fixed German text stored at the cursor and shown in the status document.
    pub fn text(self) -> String {
        match self {
            FetchError::Unreachable => "nicht erreichbar".to_owned(),
            FetchError::Status(s) => format!("HTTP {s}"),
            FetchError::Invalid => "Antwort ungültig".to_owned(),
        }
    }
}

/// Client for one source origin.
pub(crate) struct FeedClient {
    http: reqwest::Client,
    url: String,
    key: BearerKey,
}

impl std::fmt::Debug for FeedClient {
    /// URL only; the key is redacted.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedClient")
            .field("url", &self.url)
            .field("key", &self.key)
            .finish()
    }
}

/// Maps a transport error to [`FetchError::Unreachable`] and logs its cause chain (TLS, DNS, connect …) so
/// operators can see why; the request carries the key only in a header, so the message never contains it.
fn unreachable(e: reqwest::Error) -> FetchError {
    let cause = http_client::cause_chain(&e);
    tracing::warn!(%cause, "feed source not reachable");
    FetchError::Unreachable
}

impl FeedClient {
    /// Client for the validated `origin` (no trailing slash). `https_only` refuses plain http.
    pub(crate) fn new(origin: &str, key: BearerKey, https_only: bool) -> reqwest::Result<Self> {
        let http = http_client::build(CONNECT_TIMEOUT, REQUEST_TIMEOUT, https_only)?;
        Ok(Self {
            http,
            url: format!("{origin}{EVENTS_PATH}"),
            key,
        })
    }

    /// One page after `since` (`types` comma-separated, empty = all types).
    pub(crate) async fn page(
        &self,
        since: u64,
        limit: u32,
        types: &str,
    ) -> Result<Vec<u8>, FetchError> {
        let mut query = vec![("since", since.to_string()), ("limit", limit.to_string())];
        if !types.is_empty() {
            query.push(("types", types.to_owned()));
        }
        let res = self
            .http
            .get(&self.url)
            .query(&query)
            .bearer_auth(self.key.expose())
            .send()
            .await
            .map_err(unreachable)?;
        if !res.status().is_success() {
            return Err(FetchError::Status(res.status().as_u16()));
        }
        http_client::read_body(res, BODY_MAX)
            .await
            .map_err(|e| match e {
                BodyError::TooLarge => FetchError::Invalid,
                BodyError::Transport(e) => unreachable(e),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_texts() {
        assert_eq!(FetchError::Unreachable.text(), "nicht erreichbar");
        assert_eq!(FetchError::Status(503).text(), "HTTP 503");
        assert_eq!(FetchError::Invalid.text(), "Antwort ungültig");
    }

    #[test]
    fn debug_redacts_the_key() {
        let key = BearerKey::new("s3cr3t").unwrap();
        let c = FeedClient::new("https://crm.example.com", key, true).unwrap();
        let d = format!("{c:?}");
        assert!(d.contains("https://crm.example.com/fundament/v1/events"));
        assert!(!d.contains("s3cr3t"));
    }
}
