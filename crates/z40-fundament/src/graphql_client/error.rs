//! The error forms of the GraphQL client: building the client, and one query.

/// One error of the `errors` list of a GraphQL answer. It comes from the other app: treat the text as
/// data (encode it for your output) and do not assume it is free of record contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseError {
    /// The `message`, shortened to [`MAX_MESSAGE_CHARS`](crate::graphql_client::MAX_MESSAGE_CHARS)
    /// characters; empty if the answer had none.
    pub message: String,
    /// `extensions.code` (e.g. `VALIDATION`, `TIMEOUT`, `INTERNAL`) if it is a string.
    pub code: Option<String>,
}

/// Why a [`GraphqlClient`](super::GraphqlClient) could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClientBuildError {
    /// The origin is not `https://host[:port]` (or, for tests, a loopback `http://` origin).
    #[error("invalid GraphQL origin")]
    InvalidUrl,
    /// The path is not an absolute path of visible ASCII without query and fragment.
    #[error("invalid GraphQL path")]
    InvalidPath,
    /// The HTTP client could not be built (TLS backend).
    #[error("HTTP client could not be built")]
    Client,
}

/// Why a query failed. Carries no key, no request and no raw response body; GraphQL errors are the
/// parsed, bounded `errors` of the answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum QueryError {
    /// The variables could not be serialized to JSON.
    #[error("variables could not be serialized")]
    Encode,
    /// Connection or TLS problem.
    #[error("GraphQL server not reachable")]
    Unreachable,
    /// No complete answer within the timeout.
    #[error("GraphQL server did not answer in time")]
    Timeout,
    /// Non-success HTTP status (including redirects, which are never followed; `401` = wrong key,
    /// `429` = braked). `errors` are the GraphQL errors of the body if it had a readable `errors`
    /// list (e.g. `400` with `VALIDATION`), else empty.
    #[error("GraphQL server answered with HTTP {status}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// GraphQL errors of the body, if any.
        errors: Vec<ResponseError>,
    },
    /// The answer has a non-empty `errors` list. [`GraphqlClient::query`](super::GraphqlClient::query)
    /// drops partial `data`; use [`GraphqlClient::execute`](super::GraphqlClient::execute) to keep it.
    #[error("GraphQL request failed with {} error(s)", errors.len())]
    Graphql {
        /// The errors, at most [`MAX_ERRORS`](crate::graphql_client::MAX_ERRORS).
        errors: Vec<ResponseError>,
    },
    /// The body is not a GraphQL answer, or its `data` does not fit the requested type.
    #[error("GraphQL answer could not be decoded")]
    Decode,
    /// The answer is larger than the configured limit.
    #[error("GraphQL answer too large")]
    TooLarge,
}

impl QueryError {
    /// The GraphQL errors this error carries (empty for transport, decoding and size errors).
    pub fn errors(&self) -> &[ResponseError] {
        match self {
            QueryError::Status { errors, .. } | QueryError::Graphql { errors } => errors,
            _ => &[],
        }
    }

    /// Whether one of the GraphQL errors has `extensions.code == code`.
    pub fn has_code(&self, code: &str) -> bool {
        self.errors()
            .iter()
            .any(|e| e.code.as_deref() == Some(code))
    }
}
