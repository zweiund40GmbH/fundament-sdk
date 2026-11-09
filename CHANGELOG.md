# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `z40-fundament`, no feature: `StoreErrorKind` (`Retryable`, `Conflict`, `Other`) with `StoreError::kind()`,
  `StoreError::is_retryable()` and `StoreError::with_kind(kind, error)`. The MariaDB/MySQL implementations
  classify their errors: deadlock (1213), lock wait timeout (1205), I/O errors, pool timeout and a crashed
  connection worker are retryable, a duplicate key (1062) is a conflict. No sqlx type in the public API.
- `z40-fundament`, no feature: the status document types and `StatusDocument::from_json` (with
  `InvalidDocument`) live in `protocol::status` and need no feature; `status::document`, `status::StatusDocument`
  and the other old paths stay as re-exports. `status::InvalidDocument` is now also exported with `status`.
- `z40-fundament`, feature `feed-source-mysql`: `OutboxWriter::new(tables, types)` with
  `write(&mut tx, &event)` writes outbox events without a pool (same checks and `VersionExists` as before);
  `MySqlSource::write_event` delegates to it, `MySqlSource::writer()` returns it.
  `WriteEventError::is_retryable()`.
- `z40-fundament`, feature `feed-source`: `FeedEndpoint::on_failed_attempt(hook)`, called on every `401` (not on
  `429`), like `StatusEndpoint::on_failed_attempt`; `FailedAttemptHook` is shared by both and also exported from
  `feed_source`.
- `z40-fundament`, feature `status-client`: `StatusPoller` holds one HTTP client for many targets:
  `StatusPoller::new()`, `fetch(&target, &key)` and `poll_all(&targets, &keys, concurrency)`; the free
  `poll_all` uses a new poller per call. `StatusPoller::plain_http_for_tests()` and
  `StatusTarget::plain_http_for_tests(app, origin)` for loopback servers in tests.

### Changed

- `z40-fundament`, features `feed-consumer` and `status-client`: reqwest 0.13 (rustls with the aws-lc-rs
  provider and the platform verifier, plus the built-in Mozilla roots from `webpki-root-certs`; no OpenSSL).
  Timeouts, body limits and the redirect policy are unchanged.
- `z40-fundament`, feature `feed-source-mysql`: `MySqlSource::purge` measures the age of events against the
  database clock, and `offer_figures` counts the last 5 minutes and 24 hours up to the database clock (the
  `now` argument is not used), consistent with `occurred_at` stamped by the database.

### Changed (breaking)

- `z40-fundament`, feature `feed-source-mysql`: `OutboxEvent::occurred_at` is `Option<DateTime<Utc>>`.
  `OutboxEvent::state` and `OutboxEvent::tombstone` leave it `None`, and the database fills it with
  `UTC_TIMESTAMP(3)` when the event is written; `.at(t)` still sets an explicit time. Migration: code that
  builds `OutboxEvent` with a struct literal wraps the time in `Some(…)`; code that reads the field handles
  `None`; code that relied on the app clock calls `.at(Utc::now())`.

## [0.5.0] - 2026-11-08
### Added

- `z40-fundament-http`, feature `bff`: login and sessions for backend-for-frontend apps with `z40-auth` 0.5.
  - `Bff::builder(config, store, on_login)` with `cookie_name`, `base_path` (default `/api/auth`), `scopes`,
    `login_limit`, `without_login_limit`, `client_ip`, `auth_options`, `error_page`; `build()` checks the URLs
    of the configuration again. `Bff::router()` serves `GET <base>/login?returnTo=`, `GET <base>/callback`,
    `POST <base>/logout` (answers `{"logoutUrl"}` without `id_token_hint`; refused when `Sec-Fetch-Site` is
    present and not `same-origin`) and `POST <base>/backchannel-logout` (ends the sessions of `sid`, if none
    those of `sub` without `sid`; without `sid` all of `sub`).
  - Login starts limited per client address and in total (default 30 and 300 per minute, per process);
    more get `429` (`Retry-After: 60`) and write nothing; the default client address (TCP peer) warns once
    when it is loopback or private. No ID token is kept (no token in the logout URL).
  - The login hook turns the verified login into the app's session data `T` (stored as JSON) or refuses it
    with `LoginDenied`.
  - `Session<T>` extractor (`401 UNAUTHENTICATED`) and `Option<Session<T>>`, `Bff::session(&headers)`,
    `Bff::end_sessions_of(sub)`.
  - `SessionStore` trait; only SHA-256 hashes of cookie values are stored, the login state is sealed with
    AES-256-GCM under a key derived from the login cookie value.
  - Cookies `__Host-<name>_sid` (`SameSite=Strict` or `Lax`) and `__Host-<name>_login` (`Lax`, 10 minutes,
    single use); a new session id on every login.
  - Idle and absolute timeout; `return_to::sanitize` accepts only same-origin paths.
  - `BffConfig::from_lookup(prefix, lookup)`: `<PREFIX>_ISSUER`, `_CLIENT_ID`, `_CLIENT_SECRET`,
    `_REDIRECT_URI`, `_POST_LOGOUT_REDIRECT_URI`, `_COOKIE_SAMESITE`, `_SESSION_IDLE_SECS`,
    `_SESSION_MAX_SECS`, `_LOGIN_RATE_PER_IP`, `_LOGIN_RATE_TOTAL`; `BffConfig::new` validates the URLs.
- `z40-fundament-http`, feature `bff-mysql`: `MySqlSessionStore` with `SessionTables` (prefix or own names),
  `schema/mysql-session.sql` and `delete_expired(now)`.

## [0.4.0] - 2026-11-04

### Added

- New crate `z40-fundament-http` (first release): HTTP groundwork for axum 0.8 apps, configured with the
  `from_lookup` helpers of `z40-fundament`.
  - `security_headers::SecurityHeaders`: adds missing security headers (CSP, `X-Frame-Options` derived from
    `frame-ancestors`, `X-Content-Type-Options`, `Referrer-Policy`, `Permissions-Policy`,
    `Cross-Origin-Opener-Policy`, `Cross-Origin-Resource-Policy`); a CSP set by a route is kept and its
    `frame-ancestors` decides `X-Frame-Options`. `check_csp` refuses a CSP without script restriction, with an
    unsafe script source (`'unsafe-inline'`, `'unsafe-eval'`, `'unsafe-hashes'`, `*`, `data:` or another bare
    scheme), without `frame-ancestors` or with `frame-ancestors` open to every site.
    HSTS is left to the edge proxy.
  - `no_store::NoStore`: `Cache-Control: no-store` and `Pragma: no-cache` for chosen paths and prefixes,
    with exceptions. Prefixes match at path segment boundaries (`/auth` covers `/auth/x`, not `/authority`).
  - `client_ip::TrustedProxies` (`<PREFIX>_TRUSTED_PROXIES`, addresses and CIDR networks), `ClientIp`
    extractor and `resolver()` for `StatusEndpoint::client_ip` / `FeedEndpoint::client_ip`:
    `X-Forwarded-For` only from trusted peers, walked from the right, several header lines as one list,
    IPv4-mapped IPv6 peers as IPv4; prefix length 0 is refused. `ClientIp` without `ConnectInfo` warns once.
  - `csrf::Csrf`: a custom header with an app-specific value on every state-changing request (or an own
    check), protected and exempt paths and prefixes, default answer
    `403 {"code":"CSRF","message":"Anfrage abgelehnt (CSRF-Schutz). Bitte laden Sie die Seite neu."}` or the
    app's own. CORS-safelisted header names (`accept`, `content-type`, …) are refused.
  - `request-id` (feature): `RequestIdLayer` keeps an incoming `X-Request-Id` of at most 64 characters of
    `[A-Za-z0-9._:-]`, otherwise creates a UUID v7; sets it on request (header, `RequestId` extension) and
    response.
  - `trace` (feature): `trace::layer()` with a span of method, route template and request id, and status and
    latency at the end; query strings, headers and bodies are never recorded. Unmatched routes record
    `<unmatched>`; `RouteSpan::log_unmatched_path(max_len)` with `layer_with` opts in to a truncated path.
  - `telemetry` (feature): `TelemetryConfig` (`<PREFIX>_LOG`, falling back to `RUST_LOG`, then `info`;
    `<PREFIX>_LOG_FORMAT` = `json` | `text`) and an idempotent `init` returning `InitOutcome` (`Installed`,
    `InstalledWithoutLog`, `AlreadyInstalled`).
- Both crates share one exact requirement in `[workspace.dependencies]`; `just versions` checks it equals the
  workspace version.
- `docs/configuration.md` gains a generated section for `z40-fundament-http`.
- The release workflow publishes `z40-fundament-http` after `z40-fundament` and skips a crate whose version is
  already on crates.io.

## [0.3.0] - 2026-11-04

### Added

- `status-client` (new feature): `StatusClient` reads `GET /fundament/v1/status` of another app (`https` only,
  `plain_http_for_tests` for loopback; bearer key, `Accept: application/json`, 2 s for the whole request, no
  redirects, at most 256 KiB). `StatusFetchError` (`Unreachable`, `Timeout`, `Status(n)`, `Invalid`) with the
  fixed texts `nicht erreichbar`, `Zeitüberschreitung`, `HTTP <n>`, `Antwort ungültig`; transport causes are
  logged, never the key or the body. `StatusDocument::from_json` parses leniently (protocol §3.3).
  `StatusKeys::from_lookup` reads `<PREFIX>_STATUS_KEYS` (`app=key,…`, one key per app, redacted `Debug`);
  `StatusTarget` and `poll_all` poll many apps with bounded concurrency. `ConfigErrorKind::StatusKeys`.
- `docs/protocol.md` §3.3: how a client reads the status document; §3.1 gains the fixed text
  `Zeitüberschreitung` (additive).

### Changed

- `status`: reading a `StatusDocument` accepts a missing `version` or `stand` (empty string); the figures stay
  required. The written document is unchanged. The
  `document` module is also available with `status-client` alone.
- `feed-consumer`: the HTTP setup (origin check, client builder, bounded body reader, cause chain) is shared
  with the status client; an announced `Content-Length` above the page limit is refused before reading (same
  result, `Antwort ungültig`).

## [0.2.1] - 2026-11-04

### Fixed

- `feed-consumer`: the HTTP client now trusts the platform certificate store (including `SSL_CERT_FILE`) in
  addition to the built-in Mozilla roots. Before, a feed source with a certificate from a private CA failed with
  "not reachable" (TLS `UnknownIssuer`).
- `feed-consumer`: a transport failure is logged with its cause chain (TLS, DNS, connect); the fixed status text
  stays "nicht erreichbar".

## [0.2.0] - 2026-11-04

### Changed (breaking)

- `ConfigError` gains `kind: ConfigErrorKind` (`InvalidValue`, `OutOfRange { min, max }`, `InvalidUrl`,
  `KeyList(KeyListError)`, `InvalidKey(InvalidKey)`) and `source()`; `ConfigError` and `ConfigErrorKind`
  are `#[non_exhaustive]`. `name` and `reason` stay. Migration: build errors with `Lookup::invalid` or
  `Lookup::error` instead of a struct literal, and match on `kind` with a `_` arm.

### Added

- `feed-source`: `FeedEndpoint` (axum router for `GET /fundament/v1/events`: failure brake, feed key,
  query rules, cursor assignment before each answer, `hasMore` via `limit + 1`, `Cache-Control: no-store`),
  the traits `FeedSourceStore` and `SourceStatusStore`, `FeedQuery`, `build_page`, `assign_cursors_once`,
  `FeedSourceConfig::from_lookup` (`<PREFIX>_FEED_CLIENTS`, `<PREFIX>_FEED_RETENTION_DAYS`).
- `feed-source` with `status`: `source_status` fills `angebote`, `abrufer` and the note on failed feed
  attempts of 24 hours.
- `feed-source-mysql`: `MySqlSource` with `write_event` (outbox in the app's transaction,
  `WriteEventError::VersionExists` for a repeated version), cursor assigner after commit under `GET_LOCK`
  (index `(cursor, id)`, bounded wait: 5 s in the worker, 1 s before a feed answer), `purge` (keyset walk,
  keeps the newest state per record), poll log and failed-attempt counters (counted in memory, written by
  at most one task); `SourceTables` and `schema/mysql-source.sql`. Needs `binlog_format` `ROW` or `MIXED`.
- `docs/protocol.md`: errors of the feed (§2.6) and the duties of a source (§2.7), additive within version 1.
- `fields` (no feature): `text_field` and `TEXT_MAX` for IO-free domain crates; `replica` re-exports them
  at the 0.1.0 path.
- `FeedConsumerConfig::disabled()` and `Default` (the off state).
- `StatusEndpoint::on_failed_attempt` (hook on every `401`) and the type `FailedAttemptHook`.
- `MySqlStore::recent_events` accepts any integer limit, e.g. `status::MAX_EVENTS` (negative counts as 0);
  most existing calls (a `u32` or a literal) compile unchanged.
- `docs/protocol.md` §2.5: a consumer that changes `types` resyncs from `since=0`. §2.1: cursors follow the
  order of origin (the text said "commit order").
- `StoreError` is also available at the crate root (`z40_fundament::StoreError`); `consumer::StoreError`
  stays as a re-export.

## [0.1.0] - 2026-11-04

### Added

- Protocol description `docs/protocol.md` (version 1): change feed, consumer duties, status document.
- Wire types of the feed (`FeedEvent`, `FeedPage`), page validation, version rule, status `Level`.
- Health rule of a consumed source and the events on change.
- `credentials`: `KeyList` (`name=sha256hex`, constant-time verification), `FailureBrake`, `BearerKey`.
- `status`: status document types, `StatusBuilder`, axum router for `GET /fundament/v1/status`.
- `feed-consumer`: `FeedConsumer` with `run_once`, traits `RunLock`, `CursorStore`, `PageSink`,
  `FeedConsumerConfig::from_lookup`, `slot_for`.
- `replica`: `Mapping`, `KeepFields`, `ReplicaStore`, `Replica` adapter.
- `replica-mysql`: `MySqlStore` with configurable table prefix and `schema/mysql.sql`.

[Unreleased]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/zweiund40GmbH/fundament-sdk/releases/tag/v0.1.0
