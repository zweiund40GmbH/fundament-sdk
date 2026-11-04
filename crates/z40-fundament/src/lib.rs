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
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod config;
pub mod health;
pub mod protocol;
pub mod secret;

pub use secret::SecretString;
