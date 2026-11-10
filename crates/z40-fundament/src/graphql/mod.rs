//! A read-only GraphQL federation subgraph `POST /fundament/v1/graphql` (`docs/protocol.md` §4), with
//! async-graphql and axum. The app provides the schema; this module provides the protections every
//! subgraph needs the same way:
//!
//! * [`SubgraphEndpoint`] – brake → key → body limit → GraphQL request → [`precheck`] → the app's executor,
//!   with errors in GraphQL shape;
//! * [`precheck`] – the document pre-check, linear in the size of the document (fragment cycles, spreads,
//!   expansion, nesting, `_entities` representations), with [`Violation`] naming the broken limit;
//! * [`SubgraphLimits`] – all limits with their defaults, and [`SubgraphLimits::apply`] for the engine
//!   limits of the schema (depth, complexity, no introspection, `_service` kept);
//! * [`ReadBudget`] and [`ReadLimiter`] – reads per request and concurrent reads per process, for the
//!   resolvers ([`ReadBudget::from_ctx`]).
//!
//! ```rust,ignore
//! use async_graphql::{EmptyMutation, EmptySubscription, Schema};
//! use z40_fundament::graphql::{ReadBudget, SubgraphEndpoint, SubgraphLimits};
//!
//! let limits = SubgraphLimits::default();
//! let schema = limits
//!     .apply(Schema::build(Query, EmptyMutation, EmptySubscription))
//!     .data(pool)
//!     .finish();
//! let subgraph = SubgraphEndpoint::new(keys, move |request, _budget| {
//!     let schema = schema.clone();
//!     async move { schema.execute(request).await }
//! })
//! .limits(limits)
//! .brake(Arc::clone(&brake));
//! let app = axum::Router::new().merge(subgraph.router());
//!
//! // In a resolver:
//! let site = ReadBudget::from_ctx(ctx)?.read(repo::site(pool, id)).await??;
//! ```

mod budget;
mod endpoint;
mod limits;
mod precheck;
mod representations;

pub use budget::{ReadBudget, ReadLimiter};
pub use endpoint::{SubgraphCaller, SubgraphEndpoint};
pub use limits::SubgraphLimits;
pub use precheck::{Violation, precheck};
