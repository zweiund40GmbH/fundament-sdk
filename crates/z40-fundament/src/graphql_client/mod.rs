//! A client for GraphQL endpoints (a router or a subgraph) that authenticates with a static service key.
//!
//! * [`GraphqlClient`] – `POST <origin><path>` with a fixed query and variables; [`GraphqlClient::query`]
//!   (strict) and [`GraphqlClient::execute`] (keeps partial data)
//! * [`GraphqlClientOptions`] – timeout (default 10 s), answer limit (default 1 MiB), endpoint path
//! * [`QueryError`] – the one error form: transport, timeout, HTTP status, GraphQL errors, decoding, size
//! * [`ResponseError`] – a GraphQL error with its message and `extensions.code`
//!
//! Safety properties:
//!
//! * Queries are `&'static str`: they cannot be built from input; everything variable goes through
//!   `variables` (any `serde::Serialize`).
//! * The key ([`BearerKey`](crate::credentials::BearerKey)) travels only in `Authorization: Bearer`, is
//!   redacted in `Debug` and never logged. The origin must be `https://` (like the feed consumer and the
//!   status client); redirects are never followed, so the key never reaches another location.
//! * The whole request is bounded by a timeout and the answer by a byte limit (announced or sent).
//! * Errors carry no key and no raw body. GraphQL errors are parsed and bounded
//!   ([`MAX_ERRORS`] entries of [`MAX_MESSAGE_CHARS`] characters); their texts come from the server and
//!   are data, not something to put into a log line unchecked.
//!
//! **Partial data.** A GraphQL answer may carry `data` and `errors` together (a nullable field failed).
//! [`GraphqlClient::query`] treats any error as failure and drops the data. [`GraphqlClient::execute`]
//! returns both in a [`GraphqlResponse`]; fields that can fail must then be `Option`s in your type, or
//! decoding fails with [`QueryError::Decode`]. A non-success HTTP status is always an error
//! ([`QueryError::Status`]); the GraphQL errors of its body, if readable, are attached.
//!
//! ```rust,no_run
//! use serde::{Deserialize, Serialize};
//! use z40_fundament::credentials::BearerKey;
//! use z40_fundament::graphql_client::{GraphqlClient, QueryError};
//!
//! #[derive(Serialize)]
//! struct Vars<'a> {
//!     id: &'a str,
//! }
//!
//! #[derive(Deserialize)]
//! struct Data {
//!     site: Option<Site>,
//! }
//!
//! #[derive(Deserialize)]
//! struct Site {
//!     name: String,
//! }
//!
//! const SITE: &str = "query Site($id: ID!) { site(id: $id) { name } }";
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let key = BearerKey::new(std::env::var("ROUTER_KEY")?)?;
//! let client = GraphqlClient::new("https://router.example.com", key)?;
//! match client.query::<_, Data>(SITE, &Vars { id: "42" }).await {
//!     Ok(data) => println!("{:?}", data.site.map(|s| s.name)),
//!     Err(e) if e.has_code("VALIDATION") => eprintln!("the query is wrong: {e}"),
//!     Err(QueryError::Timeout | QueryError::Unreachable) => eprintln!("try again later"),
//!     Err(e) => return Err(e.into()),
//! }
//! # Ok(())
//! # }
//! ```

mod client;
mod error;
mod response;

pub use client::{
    DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, GraphqlClient, GraphqlClientOptions,
    GraphqlResponse,
};
pub use error::{ClientBuildError, QueryError, ResponseError};
pub use response::{MAX_ERRORS, MAX_MESSAGE_CHARS};
