//! HTTP client of one feed: `GET <origin>/fundament/v1/events`. Returns the raw page; parsing is pure
//! logic in [`crate::protocol::feed`]. The key goes only into the `Authorization` header; errors carry
//! the HTTP status at most, never the response body. Redirects are not followed (the key must not
//! travel to another location).

use crate::credentials::BearerKey;
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

impl FeedClient {
    /// Client for the validated `origin` (no trailing slash). `https_only` refuses plain http.
    pub(crate) fn new(origin: &str, key: BearerKey, https_only: bool) -> reqwest::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(https_only)
            .user_agent(concat!("z40-fundament/", env!("CARGO_PKG_VERSION")))
            .build()?;
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
        let mut res = self
            .http
            .get(&self.url)
            .query(&query)
            .bearer_auth(self.key.expose())
            .send()
            .await
            .map_err(|_| FetchError::Unreachable)?;
        if !res.status().is_success() {
            return Err(FetchError::Status(res.status().as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|_| FetchError::Unreachable)? {
            if body.len() + chunk.len() > BODY_MAX {
                return Err(FetchError::Invalid);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
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
