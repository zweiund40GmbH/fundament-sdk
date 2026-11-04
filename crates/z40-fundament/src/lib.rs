//! Building blocks for apps that share data over a pull-based change feed (protocol version 1,
//! `docs/protocol.md` in the repository).
//!
//! The crate is opt-in, like a database driver for an ORM: an app enables only the Cargo features it
//! needs and connects the library to its own database through small traits. Ready-made MariaDB/MySQL
//! implementations sit behind their own feature.
//!
//! Always available (no feature):
//!
//! * [`protocol`] – wire types of the feed and the status level, page validation, version rule
//! * [`health`] – health rule of a consumed source and the events on change
//! * [`config`] – `from_lookup` configuration helpers and the generated reference
//! * [`secret`] – a string type with redacted `Debug`
//!
//! Behind features:
//!
//! * `credentials` → [`credentials`] – static service keys, failure brake, outgoing bearer key
//! * `feed-consumer` → [`consumer`] – reading a feed: `FeedConsumer::run_once` and the storage traits
//! * `replica` → [`replica`] – local replica: mapping, version rule, tombstones
//! * `replica-mysql` → [`replica_mysql`] – sqlx/MariaDB implementations of the storage traits
//! * `status` → [`status`] – status document, builder and axum endpoint
//! * `feed-source` → [`feed_source`] – serving a feed: query rules, page building, axum endpoint and the
//!   [`feed_source::FeedSourceStore`] trait
//! * `feed-source-mysql` → [`feed_source_mysql`] – sqlx/MariaDB outbox, cursor assigner, retention and
//!   poll log
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod config;
#[cfg(feature = "feed-consumer")]
#[cfg_attr(docsrs, doc(cfg(feature = "feed-consumer")))]
pub mod consumer;
#[cfg(feature = "credentials")]
#[cfg_attr(docsrs, doc(cfg(feature = "credentials")))]
pub mod credentials;
#[cfg(feature = "feed-source")]
#[cfg_attr(docsrs, doc(cfg(feature = "feed-source")))]
pub mod feed_source;
#[cfg(feature = "feed-source-mysql")]
#[cfg_attr(docsrs, doc(cfg(feature = "feed-source-mysql")))]
pub mod feed_source_mysql;
pub mod health;
#[cfg(any(feature = "status", feature = "feed-source"))]
mod http;
pub mod protocol;
#[cfg(feature = "replica")]
#[cfg_attr(docsrs, doc(cfg(feature = "replica")))]
pub mod replica;
#[cfg(feature = "replica-mysql")]
#[cfg_attr(docsrs, doc(cfg(feature = "replica-mysql")))]
pub mod replica_mysql;
pub mod secret;
#[cfg(any(feature = "replica-mysql", feature = "feed-source-mysql"))]
mod sql;
#[cfg(feature = "status")]
#[cfg_attr(docsrs, doc(cfg(feature = "status")))]
pub mod status;

mod store;

pub use secret::SecretString;
pub use store::StoreError;
