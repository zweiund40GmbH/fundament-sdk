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
Ready-made MariaDB/MySQL implementations live behind their own feature. The second crate,
[`z40-fundament-http`](https://github.com/zweiund40GmbH/fundament-sdk/tree/main/crates/z40-fundament-http), holds the HTTP groundwork of axum apps
(request ids, trace layer, log setup, security headers, `no-store`, client address, CSRF guard).

**Status:** pre-release (`0.x`). The API may still change before `1.0`.

## Features

| Feature | What it gives you |
|---|---|
| *(none)* | Wire types of the feed and the status level, page validation, version rule, health rule, `from_lookup` config helpers, `SecretString` |
| `credentials` | Static service keys: verify incoming bearer keys against a `name=sha256hex` list in constant time, a per-address failure brake, an outgoing bearer key |
| `feed-consumer` | `FeedConsumer`: reads a feed page by page (`run_once`), with the traits `RunLock`, `CursorStore` and `PageSink` the app implements; `slot_for` for UTC scheduling |
| `replica` | Local replica of feed records: `Mapping` (which fields you keep), `ReplicaStore`, the `Replica` adapter (version rule, tombstones) |
| `replica-mysql` | `MySqlStore`: sqlx/MariaDB implementation of all consumer and replica traits (`GET_LOCK`, page and cursor in one transaction), schema in [`schema/mysql.sql`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/crates/z40-fundament/schema/mysql.sql) |
| `status` | Status document types and builder, and an axum router for `GET /fundament/v1/status` with key check and failure brake |
| `status-client` | Reading the status of other apps: `StatusClient` (`GET /fundament/v1/status`, 2 s deadline, no redirects, 256 KiB limit, lenient parsing, fixed error texts), `StatusKeys` (one status key per target app) and `poll_all` (bounded parallel polling) |
| `feed-source` | Serving a feed: `FeedEndpoint` (axum router for `GET /fundament/v1/events`: key check, failure brake, query rules, `hasMore`), the `FeedSourceStore` and `SourceStatusStore` traits, `assign_cursors_once` for the worker; with `status` also `source_status` (`angebote`, `abrufer`, failed attempts) |
| `feed-source-mysql` | `MySqlSource`: sqlx/MariaDB outbox written in the app's transaction, cursor assigner after commit (`GET_LOCK`), retention that keeps the newest state per record, poll log; schema in [`schema/mysql-source.sql`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/crates/z40-fundament/schema/mysql-source.sql) |

Planned: caching, GraphQL helpers and token credentials.

## HTTP groundwork: `z40-fundament-http`

```toml
[dependencies]
z40-fundament-http = { version = "0.4", features = ["request-id", "trace", "telemetry"] }
```

| Feature | What it gives you |
|---|---|
| *(none)* | `SecurityHeaders` (CSP checked: no `'unsafe-inline'` for scripts, `frame-ancestors` required; `X-Frame-Options`, `nosniff`, `Referrer-Policy`, `Permissions-Policy`, `Cross-Origin-*`; no HSTS), `NoStore`, `TrustedProxies` / `ClientIp` (`X-Forwarded-For` only from trusted proxies, also as `client_ip` function of the endpoints above), `Csrf` (custom header on state-changing requests, exempt paths, `403 {"code":"CSRF",…}` or the app's own answer) |
| `request-id` | `RequestIdLayer`: keeps a safe incoming `X-Request-Id` or creates a UUID v7, echoes it |
| `trace` | `trace::layer()`: method, route template, request id, status, latency – never query strings, headers or bodies |
| `telemetry` | `TelemetryConfig`: JSON or one-line text logs from `<PREFIX>_LOG` (unset: `RUST_LOG`, then `info`) / `<PREFIX>_LOG_FORMAT` (`json` \| `text`), idempotent `init` |

Usage and layer order: [crate README](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/crates/z40-fundament-http/README.md).

## Quick start: consume a feed into MariaDB

```toml
[dependencies]
z40-fundament = { version = "0.3", features = ["replica-mysql"] }
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

TLS: the consumer trusts the built-in Mozilla roots and the platform certificate store. A source with a
certificate from a private CA works once that CA is in the platform store or in the file named by
`SSL_CERT_FILE`. Transport failures are logged with their cause (`feed source not reachable`).

## Quick start: publish a feed from MariaDB

```toml
[dependencies]
z40-fundament = { version = "0.3", features = ["feed-source-mysql", "status"] }
```

1. Copy [`schema/mysql-source.sql`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/crates/z40-fundament/schema/mysql-source.sql) into a migration of your
   app (or print it with `SourceTables::with_prefix("app_").schema()`).
2. Write an event **in the transaction of every change**, with the complete state the consumers may see:

```rust,ignore
use z40_fundament::feed_source::{FeedEndpoint, FeedSourceConfig, assign_cursors_once};
use z40_fundament::feed_source_mysql::{MySqlSource, OutboxEvent, SourceTables};

const CUSTOMER: &str = "urn:example:crm:customer";
let source = MySqlSource::new(pool.clone(), SourceTables::with_prefix("app_")?, [CUSTOMER])?;

let mut tx = pool.begin().await?;
// … your own UPDATE of the customer, raising its version …
let state = serde_json::json!({ "name": "Example Ltd", "address": { "city": "Springfield" } });
source.write_event(&mut tx, &OutboxEvent::state(CUSTOMER, customer_id, version, state)).await?;
tx.commit().await?;
```

3. Serve the feed and run the cursor assigner and the retention in your worker:

```rust,ignore
// APP_FEED_CLIENTS (name=sha256hex per consumer), APP_FEED_RETENTION_DAYS (docs/configuration.md)
let config = FeedSourceConfig::from_lookup("APP", |k| std::env::var(k).ok())?;
let app = axum::Router::new().merge(FeedEndpoint::new(source.clone(), config.clients.clone()).router());
axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;

// Worker: every ASSIGN_INTERVAL (2 s) …
assign_cursors_once(&source).await?;
// … and once a day:
source.purge(config.retention()).await?;
```

Cursors are assigned **after commit** in the order of origin (the endpoint also runs the assigner before every
answer, waiting at most 1 s), so an event of a transaction that commits late is never skipped. Later events
wait for it, so a long transaction that writes outbox rows delays the feed by at most its own duration: keep
those transactions short. The assigner updates under `READ COMMITTED`; with binary logging use `binlog_format`
`ROW` or `MIXED` (`STATEMENT` is not supported). Each round opens one new connection (it holds `GET_LOCK` and
is closed afterwards). The retention deletes only superseded events; the newest
state of every record stays, so `since=0` is always a full initial sync. For the status document,
`source_status(builder, &source, &config.clients, now)` adds the offers per type, the last poll and backlog
of every consumer, and the failed attempts of 24 hours. Share one `FailureBrake` between the feed and the
status endpoint (`.brake(Arc::clone(&brake))`).

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

## Quick start: read the status of other apps

```toml
[dependencies]
z40-fundament = { version = "0.3", features = ["status-client"] }
```

```rust,ignore
use z40_fundament::status::{StatusClient, StatusKeys, StatusTarget, poll_all};

// MON_STATUS_KEYS=crm=<key at crm>,billing=<key at billing> (docs/configuration.md)
let keys = StatusKeys::from_lookup("MON", |k| std::env::var(k).ok())?;

// One app:
if let Some(key) = keys.get("crm") {
    match StatusClient::new("https://crm.example.com", key.clone())?.fetch().await {
        Ok(doc) => println!("{}: {:?}", doc.app, doc.state),
        Err(e) => println!("crm: {}", e.text()), // nicht erreichbar, Zeitüberschreitung, HTTP 401, Antwort ungültig
    }
}

// Many apps, at most 4 at a time; targets without a key are skipped:
let targets = [
    StatusTarget::new("crm", "https://crm.example.com")?,
    StatusTarget::new("billing", "https://billing.example.com")?,
];
for (app, result) in poll_all(&targets, &keys, 4).await? {
    // store or show `result`
}
```

A key is only ever sent to its own app. Parsing is lenient (unknown fields ignored, missing lists empty,
`docs/protocol.md` §3.3); an invalid required field makes the whole answer `Antwort ungültig`. The texts of a
document come from another app: show them as data, encoded for your output. TLS trusts the same roots as the
feed consumer (built-in plus platform store / `SSL_CERT_FILE`).

## Documentation

- [`docs/protocol.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/protocol.md) – the feed and status protocol (version 1)
- [`docs/configuration.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/configuration.md) – environment variables (generated, checked by a test)
- API documentation: `just doc` or docs.rs ([z40-fundament](https://docs.rs/z40-fundament),
  [z40-fundament-http](https://docs.rs/z40-fundament-http))
- [`CONTRIBUTING.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/CONTRIBUTING.md) – development, quality gate, release process
- [`SECURITY.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/SECURITY.md) – reporting vulnerabilities
- [`CHANGELOG.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/CHANGELOG.md)

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-APACHE) or [MIT license](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-MIT) at your
option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this
crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
