# Protocol: change feed and status (version 1)

> **Status:** published description of **version 1**. Changes that break existing consumers need a new
> version (new paths `/fundament/v2/…`); additive changes (new optional fields) are allowed within version 1.

This document describes how apps exchange records and report their health. One app **owns** a kind of record
(the *source*); other apps keep a local copy (the *consumers*). The crate `z40-fundament` implements both
sides of the feed and the status endpoint of this protocol.

Conventions: JSON bodies are UTF-8, times are UTC in RFC 3339 with seconds and `Z`
(`2026-11-04T09:00:00Z`), field names are case-sensitive. Some field names and all human-readable texts of
the status document are German; they are part of the wire format.

## 1. Principles

1. **State, not difference.** An event carries the **complete current state** of a record (only the fields the
   consumer may see) and a version number. A consumer writes the state only if the version is newer than the
   one it has. Duplicate, late and reordered events are therefore harmless.
2. **Pull is the truth.** Consumers fetch events from the source with a cursor. Nothing is pushed.
3. **Outbox in the transaction.** The source writes the event in the same transaction as the change: no event
   without a change, no change without an event.

## 2. Change feed

```
GET /fundament/v1/events?since=<cursor>&limit=<1..500>&types=<type,type>
Authorization: Bearer <key or token>
```

| Parameter | Meaning |
|---|---|
| `since` | Cursor of the last applied page (`nextCursor`); `0` reads from the beginning. |
| `limit` | Events per page, `1` to `500`. |
| `types` | Optional, comma-separated record type URNs; without it the source returns every type the caller may see. |

**Authentication.** A bearer credential per consumer: either a static key (at least 32 random bytes; the source
stores only its SHA-256 and compares in constant time) or an access token issued for the consumer. A missing or
wrong credential gives `401` without details.

**Response** `200 OK`:

```json
{
  "items": [
    {
      "cursor": 18231,
      "id": "01JD2X9V6Q8YB7M3K4N5P6R7S8",
      "type": "urn:example:crm:customer",
      "key": "0192f3a1-7c2e-7000-8000-00000000002a",
      "version": 42,
      "deleted": false,
      "occurredAt": "2026-11-04T09:12:44Z",
      "data": { "name": "Example Ltd", "address": { "city": "Springfield" } }
    }
  ],
  "nextCursor": 18231,
  "hasMore": false,
  "gap": false
}
```

### 2.1 Event

| Field | Type | Meaning |
|---|---|---|
| `cursor` | integer ≥ 1 | Position in the feed. Assigned after commit, in the order the events were written (order of origin), never reused. |
| `id` | string | Unique id of the event (for de-duplication). |
| `type` | string | Record type URN, `urn:<org>:<app>:<record>` – a type of record, not an action. |
| `key` | string | Stable id of the record, a UUID in canonical form (36 characters). |
| `version` | integer ≥ 1 | Version of the record, strictly increasing per record. |
| `deleted` | boolean | `true` = tombstone: the record was deleted, `data` is absent. Default `false`. |
| `occurredAt` | string | Time of the change. |
| `data` | object | Complete current state of the record; absent for tombstones. |

### 2.2 Page rules

- `nextCursor` is the cursor to continue from.
- `nextCursor` never goes back (`nextCursor ≥ since`), except with `gap`.
- While `hasMore` is `true`, `nextCursor` is greater than `since` (the page made progress).
- Events are in strictly ascending cursor order, each cursor is at most `nextCursor` and – except with
  `gap` – greater than `since`.
- Every event that is not a tombstone (`deleted: false`) carries `data`; every `key` is a UUID. A tombstone
  without `data` is valid.
- A page that breaks one of these rules is **invalid**: the consumer rejects the whole page, applies nothing
  of it and keeps its cursor.

### 2.3 Gap and retention

- `gap: true` means events after `since` were already purged. The consumer then reads the feed again from
  `since=0` (a full resync); stored records stay, the version rule decides what is written.
- Recommended retention, which makes `since=0` a complete initial load: purge only events that are older than
  30 days **and** superseded by a newer version of the same record. The newest state of every record (including
  a tombstone) stays.

### 2.4 Types and evolution

- New fields may appear at any time; consumers ignore unknown fields.
- Removing or redefining a field needs a new type with the suffix `.v2`; both types are delivered in parallel
  for a transition period.
- The feed only contains types and fields the caller may see. Sensitive fields (PINs, licence keys, client IP
  addresses, certificates, passwords) never appear in a feed.
- Restricting a consumer to some tenants is a filter of the source (derived from the credential), never of the
  consumer.

### 2.5 Consumer duties

- **Store the cursor durably** per source and advance it only **after** the page is applied – in the same
  transaction as the page, so the cursor never runs ahead of the data.
- **Store the version per record** and drop older or equal states. Tombstones are written like states (with
  their version); they clear the data.
- **Process idempotently.** Applying a page twice changes nothing.
- **One run at a time** per source across all instances of the consumer (e.g. a database lock).
- **On `gap`** set the cursor to `0` and read everything again (a full resync); stored states stay, the
  versions decide.
- **On any other failure** (network, HTTP status, invalid page) **keep the cursor**; the next run retries
  (default interval 60 s). A full resync would not help – the same page would come again.
- Fetch pages until `hasMore` is `false`, with a fixed upper bound of pages per run (the library: 20 by
  default), so a backlog does not block the app; the next run continues.
- Build the time slots of runs from **UTC**; slots from local time repeat or skip an hour when the clocks
  change.
- Keep only the fields you need. Never store personal data you do not need.
- **Changing `types`** (asking for more or other types than before) needs a full resync from `since=0`: the
  stored cursor may already be past events of the new types (the source skips ahead over events that did not
  match the old filter, §2.7).

### 2.6 Errors of the feed

Error bodies have the form of §3.2 (`{"code","message"}`, `Cache-Control: no-store`); a successful page
carries `Cache-Control: no-store` as well. The source checks in this order: failure brake → credential →
query.

| Status | Code | Message | When |
|---|---|---|---|
| `400` | `BAD_REQUEST` | one of the texts below | The query is invalid (§2.7). |
| `401` | `UNAUTHENTICATED` | `Anmeldung erforderlich.` | Missing or wrong credential. Header `WWW-Authenticate: Bearer`. No details. |
| `429` | `RATE_LIMITED` | `Zu viele Fehlversuche. Bitte warten Sie eine Minute.` | Failure brake, see the end of §3.2. Header `Retry-After: 60`. |
| `500` | `INTERNAL` | `Interner Fehler. Bitte versuchen Sie es später erneut.` | The events could not be read. |

Messages of `400`: `Ungültige Abfrageparameter.`, `Jeder Parameter darf nur einmal vorkommen.`,
`„since“ muss eine ganze Zahl ab 0 sein.`, `„limit“ muss zwischen 1 und 500 liegen.`,
`Unbekannter Typ in „types“.`

### 2.7 Source duties

- **Query.** A missing or empty `since` is `0`; a missing or empty `limit` is `100`; a missing or empty
  `types` means every type the caller may see. A type the source does not offer, a parameter given twice, a
  `since` that is not a non-negative integer or a `limit` outside `1..500` is `400`. Unknown parameters are
  ignored.
- **Outbox in the transaction** (§1): the event is written in the transaction of the change.
- **Cursor after commit.** Cursors are assigned to committed events only, in the order of origin, from a
  counter that is never reset (never reused, also after the retention deleted events). An event of a still
  open transaction is never skipped: later events wait for it (a bounded time per round), and it gets a
  cursor once it is committed. All cursors of one assignment become visible at once. A long transaction that
  writes events therefore delays the feed by at most its own duration – keep such transactions short. A feed
  request waits only briefly for them (the library: 1 s; its worker 5 s).
- **Page.** Events with a cursor above `since`, ascending, at most `limit`; `hasMore` is `true` when more
  matching events exist (e.g. read `limit + 1`). `nextCursor` is the cursor of the last event, or `since`
  for an empty page. Without `hasMore` it may move further, up to the last cursor assigned before the read,
  when no event in between matches `types` – so a consumer of rare types does not scan the same events again.
- **`id`** is unique per event within the source (e.g. the outbox row id); tombstones carry no `data`.
- **Retention** as recommended in §2.3; then `gap` is never `true`.
- **Status.** The source remembers the last poll of each consumer (time and `nextCursor`) for `abrufer`
  (§3) and counts failed credential attempts for the note `<n> Fehlversuche am Feed in 24 Stunden` (one:
  `1 Fehlversuch am Feed in 24 Stunden`), level `warnung`, which does not change `zustand`.

## 3. Status

Every app describes its health for monitoring. Only counters, times and fixed texts – **no personal data**, no
record contents, no keys.

```
GET /fundament/v1/status
Authorization: Bearer <status key or token>
```

The status credentials are **separate** from the feed credentials (a feed consumer does not learn who else
polls).

**Response** `200 OK`, `Cache-Control: no-store`, within 2 seconds:

```json
{
  "app": "billing", "version": "1.4.0", "stand": "2026-11-04T10:00:00Z",
  "zustand": "ok",
  "hinweise": [ { "stufe": "warnung", "text": "3 Fehlversuche am Feed in 24 Stunden" } ],
  "angebote": [ { "art": "feed", "typ": "urn:example:billing:invoice", "letzterCursor": 18231,
                  "ereignisseProMinute": 0.4, "ereignisse24h": 512 } ],
  "abrufer":  [ { "name": "crm", "art": "feed", "letzterAbruf": "2026-11-04T09:59:30Z",
                  "cursor": 18200, "rueckstand": 31, "fehlversuche24h": null } ],
  "bezieht":  [ { "quelle": "crm", "art": "feed", "letzterAbruf": "2026-11-04T09:59:00Z",
                  "zustand": "ok", "text": null } ],
  "ereignisse": [ { "zeit": "2026-11-04T09:55:00Z", "art": "bezug", "stufe": "ok",
                    "text": "Feed von crm wieder erreichbar" } ]
}
```

| Field | Meaning |
|---|---|
| `app` | Name of the app. |
| `version` | Version of the app. |
| `stand` | Time the document was built. |
| `zustand` | Overall level: `ok`, `warnung` or `fehler`. |
| `hinweise[]` | Notes: `stufe` (level) and `text`. |
| `angebote[]` | What the app offers, e.g. one entry per feed type: `art`, `typ`, `letzterCursor`, `ereignisseProMinute` (one decimal), `ereignisse24h`. |
| `abrufer[]` | Who polls the app (by the name of the credential): `name`, `art`, `letzterAbruf` (or `null`), `cursor` of that poll (or `null`), `rueckstand` (events after that cursor, counted up to 10 000, above reported as `10000`), `fehlversuche24h` (failed attempts of this caller, `null` when they cannot be attributed – then the total appears as a note). |
| `bezieht[]` | Sources the app reads from: `quelle`, `art`, `letzterAbruf`, `zustand`, `text` (fixed error text or `null`). |
| `ereignisse[]` | The newest at most 100 events of the app (syncs, imports, changes of a source between ok and failing), newest first, kept 30 days: `zeit`, `art` (free), `stufe`, `text`. |

Texts are German, for humans, and never contain names, e-mail addresses or ids of persons.

A consumed feed (`bezieht[]` with `art: "feed"`) counts as `fehler` when it never ran, when its last run
failed, or when its last successful run is older than 15 minutes (the threshold is configurable). `zustand`
of the app is the worst level of its sources and of the notes that count.

### 3.1 Fixed texts

The fixed texts (`text` of `bezieht`, `hinweise` and `ereignisse`, and `message` of errors) are German and
**part of version 1**. Monitoring shows them unchanged but does not interpret them; machines use the levels
and codes. The texts of a consumed feed are:

| Text | Meaning |
|---|---|
| `noch kein Abruf` | The feed never ran. |
| `nicht erreichbar` | Connection, TLS or timeout problem. |
| `HTTP <status>`, e.g. `HTTP 503` | The source answered with a non-success status. |
| `Antwort ungültig` | The page was invalid (§2.2) or too large. |
| `Zeitüberschreitung` | Reading a status (§3.3): no complete answer within 2 seconds. |
| `Abruf fehlgeschlagen` | The last run failed without a stored text. |
| `kein erfolgreicher Abruf seit über <n> Minuten` | The last success is older than the threshold. |

A failing feed adds the note `Feed von <source>: <text>`. Events on a change of a feed are
`Feed von <source>: <text>` (level `fehler`) and `Feed von <source> wieder erreichbar` (level `ok`).

### 3.2 Errors

All error bodies have the form `{"code": "<CODE>", "message": "<text>"}` and `Cache-Control: no-store`.

| Status | Code | Message | When |
|---|---|---|---|
| `401` | `UNAUTHENTICATED` | `Anmeldung erforderlich.` | Missing or wrong credential. Header `WWW-Authenticate: Bearer`. No details. |
| `429` | `RATE_LIMITED` | `Zu viele Fehlversuche. Bitte warten Sie eine Minute.` | More than 10 failed attempts from one address (IPv6: one /64) within a minute. Header `Retry-After: 60`. |
| `500` | `INTERNAL` | `Interner Fehler. Bitte versuchen Sie es später erneut.` | The status could not be built in time. |

The **source** brakes failed attempts the same way on its change feed (§2): more than 10 per address (IPv6:
one /64) and minute → `429` with `Retry-After: 60`. The client address is the direct peer, or the address a
trusted reverse proxy forwards – `X-Forwarded-For` from anyone else is ignored.

### 3.3 Reading the status

A client (e.g. monitoring) reads the document leniently, so apps can add fields and send less:

- Unknown fields are ignored.
- Missing lists (`hinweise`, `angebote`, `abrufer`, `bezieht`, `ereignisse`) count as empty; missing `version`
  and `stand` as unknown (an empty string); missing `null`able fields (`letzterAbruf`, `cursor`,
  `fehlversuche24h`, `text` of `bezieht`) as `null`.
- Required are `app` (not empty) and `zustand`, and in each entry `stufe`, `text`, `art`, `typ`, `name`,
  `quelle`, `zustand` and `zeit` where listed above, as well as the figures `letzterCursor`,
  `ereignisseProMinute`, `ereignisse24h` and `rueckstand` (every producer sends them, so `0` always means 0). A required field that is
  missing or invalid (an unknown level, a time that is not RFC 3339, a negative figure) makes the **whole** answer invalid (`Antwort ungültig`).
- The client sends each app only the status key of that app, follows no redirects, waits at most 2 seconds for
  the complete answer and reads at most 256 KiB. Its fixed texts are those of §3.1 (`nicht erreichbar`,
  `HTTP <status>`, `Antwort ungültig`, `Zeitüberschreitung`).
- Texts are data from another app: shown encoded, never interpreted.
