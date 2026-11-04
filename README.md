# fundament-sdk

[![CI](https://github.com/zweiund40GmbH/fundament-sdk/actions/workflows/ci.yml/badge.svg)](https://github.com/zweiund40GmbH/fundament-sdk/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/z40-fundament.svg)](https://crates.io/crates/z40-fundament)
[![docs.rs](https://img.shields.io/docsrs/z40-fundament)](https://docs.rs/z40-fundament)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Building blocks for apps that share data over a **pull-based change feed**: one app owns a kind of record
and publishes its changes; other apps keep a local copy and report their health in a common status
document. The wire format is described in [`docs/protocol.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/protocol.md) (version 1).

The Rust crate [`z40-fundament`](https://github.com/zweiund40GmbH/fundament-sdk/tree/main/crates/z40-fundament) is opt-in, like a database driver for an ORM: an app
enables only the Cargo features it needs and connects the library to its own database through small traits.
Ready-made MariaDB/MySQL implementations live behind their own feature.

**Status:** pre-release (`0.1.0`, unreleased). The API may still change before `1.0`.

## Features

| Feature | What it gives you |
|---|---|
| *(none)* | Wire types of the feed and the status level, page validation, version rule, health rule, `from_lookup` config helpers, `SecretString` |
| `credentials` | Static service keys: verify incoming bearer keys against a `name=sha256hex` list in constant time, a per-address failure brake, an outgoing bearer key |
| `feed-consumer` | `FeedConsumer`: reads a feed page by page (`run_once`), with the traits `RunLock`, `CursorStore` and `PageSink` the app implements; `slot_for` for UTC scheduling |
| `replica` | Local replica of feed records: `Mapping` (which fields you keep), `ReplicaStore`, the `Replica` adapter (version rule, tombstones) |
| `replica-mysql` | `MySqlStore`: sqlx/MariaDB implementation of all consumer and replica traits (`GET_LOCK`, page and cursor in one transaction), schema in [`schema/mysql.sql`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/crates/z40-fundament/schema/mysql.sql) |
| `status` | Status document types and builder, and an axum router for `GET /fundament/v1/status` with key check and failure brake |

Planned: publishing a feed (`feed-source`), caching, GraphQL helpers, a status client and token credentials.

## Quick start: consume a feed into MariaDB

```toml
[dependencies]
z40-fundament = { version = "0.1", features = ["replica-mysql"] }
```

1. Copy [`schema/mysql.sql`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/crates/z40-fundament/schema/mysql.sql) into a migration of your app (add your
   table prefix, or print the statements with `Tables::with_prefix("app_").schema()`). The library never runs
   migrations in your database.
2. Decide which fields you keep. By default nothing of `data` is stored; `KeepFields` keeps named fields:

```rust,ignore
use z40_fundament::consumer::{FeedConsumer, FeedConsumerConfig};
use z40_fundament::replica::{KeepFields, Replica};
use z40_fundament::replica_mysql::{MySqlStore, Tables};

// CRM_FEED_URL, CRM_FEED_KEY, CRM_FEED_INTERVAL_SECS (docs/configuration.md)
let config = FeedConsumerConfig::from_lookup("CRM_FEED", |k| std::env::var(k).ok())?;
let Some(consumer) = FeedConsumer::from_config("crm", &config)?
    .map(|b| b.types(["urn:example:crm:customer"]).build())
    .transpose()?
else {
    return Ok(()); // feed not configured
};
let mapping = KeepFields::new().keep("urn:example:crm:customer", "customer", &["name", "address.city"])?;
let store = Replica::new(MySqlStore::new(pool, Tables::with_prefix("app_")?), mapping);

// In your worker, once per slot (`slot_for(Utc::now(), interval)` de-duplicates across workers):
let report = consumer.run_once(&store).await?;
```

A failure of the feed (network, HTTP status, invalid page) is recorded at the cursor with a fixed text and is
part of the report; only a failure of your own database is an `Err`. The pool needs at least 2 connections
(one holds the run lock).

## Quick start: status endpoint

```rust,ignore
use z40_fundament::status::{StatusConfig, StatusEndpoint};

let config = StatusConfig::from_lookup("APP", |k| std::env::var(k).ok())?; // APP_STATUS_CLIENTS
let app = axum::Router::new().merge(StatusEndpoint::new(my_provider, config.clients).router());
// The failure brake needs the client address:
axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
```

Behind a reverse proxy pass `StatusEndpoint::client_ip(|parts| …)` a function that reads `X-Forwarded-For`
**only** when the direct peer is one of your proxies; otherwise every client could pick its own address and
dodge the brake. Without any address all clients share one brake (a warning is logged once).

## Documentation

- [`docs/protocol.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/protocol.md) – the feed and status protocol (version 1)
- [`docs/configuration.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/configuration.md) – environment variables (generated, checked by a test)
- API documentation: `just doc` or [docs.rs](https://docs.rs/z40-fundament)
- [`CONTRIBUTING.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/CONTRIBUTING.md) – development, quality gate, release process
- [`SECURITY.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/SECURITY.md) – reporting vulnerabilities
- [`CHANGELOG.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/CHANGELOG.md)

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-APACHE) or [MIT license](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-MIT) at your
option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this
crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
