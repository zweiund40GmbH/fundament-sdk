# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Planned as 0.2.0 (new features; the 0.1.0 API is unchanged).

### Added

- `feed-source`: `FeedEndpoint` (axum router for `GET /fundament/v1/events`: failure brake, feed key,
  query rules, cursor assignment before each answer, `hasMore` via `limit + 1`, `Cache-Control: no-store`),
  the traits `FeedSourceStore` and `SourceStatusStore`, `FeedQuery`, `build_page`, `assign_cursors_once`,
  `FeedSourceConfig::from_lookup` (`<PREFIX>_FEED_CLIENTS`, `<PREFIX>_FEED_RETENTION_DAYS`).
- `feed-source` with `status`: `source_status` fills `angebote`, `abrufer` and the note on failed feed
  attempts of 24 hours.
- `feed-source-mysql`: `MySqlSource` with `write_event` (outbox in the app's transaction), cursor assigner
  after commit under `GET_LOCK`, `purge` (keeps the newest state per record), poll log and failed-attempt
  counters; `SourceTables` and `schema/mysql-source.sql`.
- `docs/protocol.md`: errors of the feed (§2.6) and the duties of a source (§2.7), additive within version 1.
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

[Unreleased]: https://github.com/zweiund40GmbH/fundament-sdk/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/zweiund40GmbH/fundament-sdk/releases/tag/v0.1.0
