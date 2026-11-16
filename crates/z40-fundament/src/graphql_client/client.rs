//! The client: one fixed query with variables per request, sent with a service key.

use super::error::{ClientBuildError, QueryError};
use super::response::{self, Parsed};
use crate::credentials::BearerKey;
use crate::http_client::{self, BodyError};
use crate::protocol::GRAPHQL_PATH;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

/// Default time limit of one query, from connecting until the body is read.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default largest accepted answer.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Time limit of connecting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest body that is read from a non-success answer to find its GraphQL errors.
const ERROR_BODY_MAX: usize = 16 * 1024;

/// Limits and target of a [`GraphqlClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlClientOptions {
    /// Time limit of one query, from connecting until the body is read (default 10 s).
    pub timeout: Duration,
    /// Largest accepted answer in bytes (default 1 MiB); a larger one is [`QueryError::TooLarge`].
    pub max_response_bytes: usize,
    /// The path of the endpoint (default [`GRAPHQL_PATH`], the subgraph of this library; a router
    /// usually serves `/` or `/graphql`).
    pub path: String,
}

impl Default for GraphqlClientOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            path: GRAPHQL_PATH.to_owned(),
        }
    }
}

/// The answer of [`GraphqlClient::execute`]: the typed `data` and the errors next to it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct GraphqlResponse<T> {
    /// The `data` of the answer; `None` if it was missing or `null` (then `errors` is not empty).
    pub data: Option<T>,
    /// The `errors` of the answer, bounded in number and length.
    pub errors: Vec<super::ResponseError>,
}

/// The request body: `variables` is left out when it serializes to `null` (e.g. `()`).
#[derive(Serialize)]
struct RequestBody<'a> {
    query: &'static str,
    #[serde(skip_serializing_if = "Value::is_null")]
    variables: &'a Value,
}

/// Client for one GraphQL endpoint. Cheap to clone (the connection pool is shared). The key is sent
/// only as `Authorization: Bearer`, never in the URL, and is redacted in `Debug`.
#[derive(Clone)]
pub struct GraphqlClient {
    http: reqwest::Client,
    url: String,
    key: BearerKey,
    max_response_bytes: usize,
}

impl std::fmt::Debug for GraphqlClient {
    /// URL only; the key is redacted.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphqlClient")
            .field("url", &self.url)
            .field("key", &self.key)
            .finish()
    }
}

impl GraphqlClient {
    /// Client for the `https://` `origin` (one trailing `/` allowed) with the default
    /// [`GraphqlClientOptions`], sending `key`.
    pub fn new(origin: &str, key: BearerKey) -> Result<Self, ClientBuildError> {
        Self::with_options(origin, key, GraphqlClientOptions::default())
    }

    /// Like [`GraphqlClient::new`] with `options`.
    pub fn with_options(
        origin: &str,
        key: BearerKey,
        options: GraphqlClientOptions,
    ) -> Result<Self, ClientBuildError> {
        let origin = http_client::https_origin(origin).ok_or(ClientBuildError::InvalidUrl)?;
        Self::build(&origin, key, options, true)
    }

    /// Like [`GraphqlClient::with_options`], but for a plain `http://` origin on a **loopback** address
    /// (a local server in tests; the same escape hatch as `StatusClient::plain_http_for_tests`).
    pub fn plain_http_for_tests(
        origin: &str,
        key: BearerKey,
        options: GraphqlClientOptions,
    ) -> Result<Self, ClientBuildError> {
        let origin =
            http_client::loopback_http_origin(origin).ok_or(ClientBuildError::InvalidUrl)?;
        Self::build(&origin, key, options, false)
    }

    /// The client for a validated `origin`.
    fn build(
        origin: &str,
        key: BearerKey,
        options: GraphqlClientOptions,
        https_only: bool,
    ) -> Result<Self, ClientBuildError> {
        let path = &options.path;
        if !path.starts_with('/')
            || path.starts_with("//")
            || !path
                .bytes()
                .all(|b| b.is_ascii_graphic() && b != b'?' && b != b'#')
        {
            return Err(ClientBuildError::InvalidPath);
        }
        let http = http_client::build(CONNECT_TIMEOUT, options.timeout, https_only)
            .map_err(|_| ClientBuildError::Client)?;
        Ok(Self {
            http,
            url: format!("{origin}{path}"),
            key,
            max_response_bytes: options.max_response_bytes,
        })
    }

    /// Runs `query` with `variables` and returns its `data` as `T`.
    ///
    /// Strict: any entry in `errors` is [`QueryError::Graphql`], and a partial `data` next to it is
    /// dropped. An answer without `data` (missing or `null`) and without errors is
    /// [`QueryError::Decode`], also for `T = ()` or an `Option`. Use [`GraphqlClient::execute`] when
    /// partial results are useful. `variables` must serialize to a JSON object or `null`, else
    /// [`QueryError::Encode`]; pass `&()` for a query without variables.
    pub async fn query<V, T>(&self, query: &'static str, variables: &V) -> Result<T, QueryError>
    where
        V: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let parsed = self.send(query, variables).await?;
        if !parsed.errors.is_empty() {
            return Err(QueryError::Graphql {
                errors: parsed.errors,
            });
        }
        // `data` missing or `null` without errors is not a valid answer (GraphQL spec).
        let data = parsed.data.ok_or(QueryError::Decode)?;
        serde_json::from_value(data).map_err(|_| QueryError::Decode)
    }

    /// Like [`GraphqlClient::query`], but keeps partial results: GraphQL errors do not fail the call,
    /// they are returned next to the `data`. `data` is `None` only if the answer had none (then there
    /// are errors); a `data` that does not fit `T` is [`QueryError::Decode`], so make the fields of
    /// nullable GraphQL fields `Option`s.
    pub async fn execute<V, T>(
        &self,
        query: &'static str,
        variables: &V,
    ) -> Result<GraphqlResponse<T>, QueryError>
    where
        V: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let Parsed { data, errors } = self.send(query, variables).await?;
        let data = match data {
            None if errors.is_empty() => return Err(QueryError::Decode),
            None => None,
            some => Some(decode(some).ok_or(QueryError::Decode)?),
        };
        Ok(GraphqlResponse { data, errors })
    }

    /// POSTs the request and returns the parsed answer of a success status.
    async fn send<V: Serialize + ?Sized>(
        &self,
        query: &'static str,
        variables: &V,
    ) -> Result<Parsed, QueryError> {
        let variables = serde_json::to_value(variables).map_err(|_| QueryError::Encode)?;
        if !matches!(variables, Value::Null | Value::Object(_)) {
            return Err(QueryError::Encode);
        }
        let body = serde_json::to_vec(&RequestBody {
            query,
            variables: &variables,
        })
        .map_err(|_| QueryError::Encode)?;
        let res = self
            .http
            .post(&self.url)
            .bearer_auth(self.key.expose())
            .header(
                reqwest::header::ACCEPT,
                "application/graphql-response+json, application/json",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        let status = res.status();
        if !status.is_success() {
            return Err(self.failed_status(res).await);
        }
        let body = http_client::read_body(res, self.max_response_bytes)
            .await
            .map_err(|e| match e {
                BodyError::TooLarge => {
                    tracing::warn!(url = %self.url, "graphql: answer too large");
                    QueryError::TooLarge
                }
                BodyError::Transport(e) => self.transport(e),
            })?;
        response::parse(&body).map_err(|()| {
            tracing::warn!(url = %self.url, "graphql: answer is not a GraphQL response");
            QueryError::Decode
        })
    }

    /// The error of a non-success `res`: its status plus the GraphQL errors of a small, readable body.
    async fn failed_status(&self, res: reqwest::Response) -> QueryError {
        let status = res.status().as_u16();
        let limit = ERROR_BODY_MAX.min(self.max_response_bytes);
        let errors = match http_client::read_body(res, limit).await {
            Ok(body) => response::parse(&body).map(|p| p.errors).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        tracing::warn!(url = %self.url, status, "graphql: HTTP error status");
        QueryError::Status { status, errors }
    }

    /// Maps a transport error to `Timeout` or `Unreachable` and logs its cause chain.
    fn transport(&self, e: reqwest::Error) -> QueryError {
        let cause = http_client::cause_chain(&e);
        if e.is_timeout() {
            tracing::warn!(url = %self.url, %cause, "graphql: no answer in time");
            QueryError::Timeout
        } else {
            tracing::warn!(url = %self.url, %cause, "graphql: server not reachable");
            QueryError::Unreachable
        }
    }
}

/// `data` as `T`; `None` if it is missing or does not fit.
fn decode<T: DeserializeOwned>(data: Option<Value>) -> Option<T> {
    serde_json::from_value(data.unwrap_or(Value::Null)).ok()
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
