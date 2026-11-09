# Configuration reference

Every setting is read through a `from_lookup(prefix, lookup)` function: `lookup` is any
`Fn(&str) -> Option<String>` (usually `|k| std::env::var(k).ok()`), and the app chooses the prefix. With the
prefix `CRM_FEED`, the variable `<PREFIX>_URL` is `CRM_FEED_URL`.

Values are trimmed; an empty value counts as unset. Error messages name the variable but never echo its value.
The crate `z40-fundament-http` reads its settings the same way (section below).

The tables below are generated from the code (`just config-doc`); a test fails when they are outdated.

<!-- BEGIN GENERATED: just config-doc -->
### Feed consumer (`feed-consumer`)

Read with `FeedConsumerConfig::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_URL` | (unset) | Origin of the source app, `https://host[:port]` without path. Unset: the feed is off. Processes that only report the status may get the URL without the key. |
| `<PREFIX>_KEY` | (unset) | Feed key of this app at the source (sent as `Authorization: Bearer`). Unset: the feed is off. Secret: only for the process that runs the consumer. |
| `<PREFIX>_INTERVAL_SECS` | `60` | Seconds between runs, 30–3600. Used by the app's scheduler (`slot_for`). |

### Feed source (`feed-source`)

Read with `FeedSourceConfig::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_FEED_CLIENTS` | (unset) | Accepted feed keys as comma-separated `name=sha256hex` entries (only the SHA-256 of a key, never the key); the name identifies the consumer in the status document. Unset or empty: every feed request gets `401`. Keep it separate from status keys. |
| `<PREFIX>_FEED_RETENTION_DAYS` | `30` | Retention of superseded events, 7–3650 days: an event is deleted once a newer version of the same record exists and its own `occurred_at` is older than this. The newest state of every record (also a tombstone) is always kept. |

### Status endpoint (`status`)

Read with `StatusConfig::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_STATUS_CLIENTS` | (unset) | Accepted status keys as comma-separated `name=sha256hex` entries (only the SHA-256 of a key, never the key). Unset or empty: every status request gets `401`. Keep it separate from feed keys. |

### Status client (`status-client`)

Read with `StatusKeys::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_STATUS_KEYS` | (unset) | Status keys of the apps this app polls, as comma-separated `app=key` entries (one key per app; a key is only sent to its own app). Unset or empty: no app is polled. Secret. |

<!-- END GENERATED -->

## z40-fundament-http

<!-- BEGIN GENERATED: just config-doc (z40-fundament-http) -->
### Client address (`z40-fundament-http`, always on)

Read with `TrustedProxies::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_TRUSTED_PROXIES` | (unset) | Reverse proxies whose `X-Forwarded-For` is believed, as comma-separated addresses or CIDR networks (`10.0.0.0/8, fd00::/8`). Unset or empty: no proxy is trusted and the TCP peer is the client. List only your own proxies: every listed address can choose the client address, so a broad range makes `X-Forwarded-For` spoofable (prefix length 0 is refused). |

### Log setup (`z40-fundament-http`, feature `telemetry`)

Read with `TelemetryConfig::from_lookup(prefix, lookup)`, or `from_lookup_with(prefix, lookup, Defaults { filter, format })` for the app's own defaults.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_LOG` | `info` | Log filter as `EnvFilter` directive, e.g. `info,sqlx=warn` or `debug`. Unset or empty: `RUST_LOG`, then `info` (or the app's default). An invalid directive is a configuration error. |
| `<PREFIX>_LOG_FORMAT` | `json` | `json` (one JSON object per line, for log collectors) or `text` (readable text, one line per event, for local work). Unset or empty: `json` (or the app's default). |

### Login and sessions (`z40-fundament-http`, feature `bff`)

Read with `BffConfig::from_lookup(prefix, lookup)`, or `from_lookup_with(prefix, lookup, Defaults { client_id, redirect_uri, post_logout_redirect_uri, allow_loopback_http })` for defaults the app computes. Apps that already use `AUTH_ISSUER`, `AUTH_CLIENT_ID`, … pass the prefix `AUTH`. Plain-HTTP loopback URLs are refused unless the app allows them (`allow_loopback_http`, development only).

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_ISSUER` | (unset) | Issuer of the login provider (`https://…`; `http://` only for loopback hosts when the app allows it in development), exactly as in its discovery document. Required. |
| `<PREFIX>_CLIENT_ID` | (unset) | Client id of this app at the login provider. Required unless the app passes a default (`from_lookup_with`). |
| `<PREFIX>_CLIENT_SECRET` | (unset) | Client secret of this app. Required. Secret. |
| `<PREFIX>_REDIRECT_URI` | (unset) | Callback URL of this app (`https://app.example.com/api/auth/callback`), exactly as registered. Required unless the app passes a default (`from_lookup_with`). A plain-HTTP loopback URL (only when the app allows it in development) turns off `Secure` and the `__Host-` prefix of the cookies. |
| `<PREFIX>_POST_LOGOUT_REDIRECT_URI` | (unset) | Where the login provider sends the browser after the logout, exactly as registered. Unset: the app's default (`from_lookup_with`), else the provider's own page. |
| `<PREFIX>_COOKIE_SAMESITE` | `strict` | `SameSite` of the session cookie: `strict` (links from other sites open the app without the session) or `lax`. The login cookie is always `lax`. |
| `<PREFIX>_SESSION_IDLE_SECS` | `7200` | A session ends after this many seconds without a request, 60–86400 (cut to the absolute timeout). |
| `<PREFIX>_SESSION_MAX_SECS` | `36000` | A session ends this many seconds after the login, however active, 300–2592000. Bounds how long roles taken at login stay in effect without a back-channel logout. |
| `<PREFIX>_LOGIN_RATE_PER_IP` | `30` | Login starts per client address and minute, 1–10000 (more get `429`). Counted per process; IPv6 by /64. |
| `<PREFIX>_LOGIN_RATE_TOTAL` | `300` | Login starts per minute from all clients together, 1–1000000. Bounds the rows written before anyone is authenticated. Counted per process. |
<!-- END GENERATED (z40-fundament-http) -->
