# Configuration reference

Every setting is read through a `from_lookup(prefix, lookup)` function: `lookup` is any
`Fn(&str) -> Option<String>` (usually `|k| std::env::var(k).ok()`), and the app chooses the prefix. With the
prefix `CRM_FEED`, the variable `<PREFIX>_URL` is `CRM_FEED_URL`.

Values are trimmed; an empty value counts as unset. Error messages name the variable but never echo its value.

The tables below are generated from the code (`just config-doc`); a test fails when they are outdated.

<!-- BEGIN GENERATED: just config-doc -->
### Feed consumer (`feed-consumer`)

Read with `FeedConsumerConfig::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_URL` | (unset) | Origin of the source app, `https://host[:port]` without path. Unset: the feed is off. Processes that only report the status may get the URL without the key. |
| `<PREFIX>_KEY` | (unset) | Feed key of this app at the source (sent as `Authorization: Bearer`). Unset: the feed is off. Secret: only for the process that runs the consumer. |
| `<PREFIX>_INTERVAL_SECS` | `60` | Seconds between runs, 30–3600. Used by the app's scheduler (`slot_for`). |

### Status endpoint (`status`)

Read with `StatusConfig::from_lookup(prefix, lookup)`.

| Variable | Default | Description |
|---|---|---|
| `<PREFIX>_STATUS_CLIENTS` | (unset) | Accepted status keys as comma-separated `name=sha256hex` entries (only the SHA-256 of a key, never the key). Unset or empty: every status request gets `401`. Keep it separate from feed keys. |

<!-- END GENERATED -->
