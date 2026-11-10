# Protocol: change feed, status and subgraph (version 1)

> **Status:** published description of **version 1**. Changes that break existing consumers need a new
> version (new paths `/fundament/v2/…`); additive changes (new optional fields) are allowed within version 1.

This document describes how apps exchange records and report their health. One app **owns** a kind of record
(the *source*); other apps keep a local copy (the *consumers*). The crate `z40-fundament` implements both
sides of the feed, the status endpoint and the checks of a federation subgraph (§4) of this protocol.

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
and codes. The texts of a consumed feed (in `bezieht`) and of a client reading a status (§3.3) are:

| Text | Meaning |
|---|---|
| `noch kein Abruf` | The feed never ran. |
| `nicht erreichbar` | Connection, TLS or DNS problem. The feed consumer also reports a timeout this way; the status client reports a timeout as `Zeitüberschreitung`. |
| `HTTP <status>`, e.g. `HTTP 503` | The source answered with a non-success status. |
| `Antwort ungültig` | The page was invalid (§2.2) or too large. |
| `Zeitüberschreitung` | Status client only (§3.3): no complete answer within 2 seconds (also when connecting takes too long). |
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
  and `stand` as unknown (an empty string); `null` counts as missing for these lists, `version` and `stand`.
  Missing `null`able fields (`letzterAbruf`, `cursor`, `fehlversuche24h`, `text` of `bezieht`) read as `null`.
- Required are `app` (not empty) and `zustand`, and in each entry `stufe`, `text`, `art`, `typ`, `name`,
  `quelle`, `zustand` and `zeit` where listed above, as well as the figures `letzterCursor`,
  `ereignisseProMinute`, `ereignisse24h` and `rueckstand` (every producer sends them, so `0` always means 0;
  `null` is invalid). A required field that is missing or invalid (an unknown level, a time that is not
  RFC 3339, a negative figure) makes the **whole** answer invalid (`Antwort ungültig`).
- The client sends each app only the status key of that app, follows no redirects, waits at most 2 seconds for
  the complete answer and reads at most 256 KiB. Its fixed texts are those of §3.1 (`nicht erreichbar`,
  `HTTP <status>`, `Antwort ungültig`, `Zeitüberschreitung`).
- Texts are data from another app: shown encoded, never interpreted.

## 4. Federation subgraph

An app may serve a **read-only GraphQL subgraph** for a federation router (Apollo Federation v2). The router
sends small entity fetches (`_entities`) and reads the schema (`_service { sdl }`); it does not pass `_entities`
on to its own clients. The subgraph is a separate schema of the app, without mutations. The crate
`z40-fundament` (feature `graphql`) implements the checks below; the app provides the schema.

```
POST /fundament/v1/graphql
Authorization: Bearer <subgraph key>
Content-Type: application/json

{"query": "query($r: [_Any!]!) { _entities(representations: $r) { ... on Site { name } } }",
 "variables": {"r": [{"__typename": "Site", "id": "s-1"}]}}
```

Only `POST`; other methods get `405` with `Allow: POST` (§4.5). The subgraph keys are a list of their own (the same `name=sha256hex` form as the feed and status
keys); which callers get one is the app's choice. Failed attempts count at the **same failure brake** as the
feed and the status endpoint of the app (§3.2).

### 4.1 Order of the checks

1. **Failure brake** (§3.2) – before anything else.
2. **Key** – before the body is read, so a request without a valid key never buffers a body (a request
   without a key and with a body over the limit gets `401`, not `413`).
3. **Body** – at most the body limit (default 16 KiB). Router fetches are small; parsing is synchronous work
   that the deadline does not stop (a worst-case body of 64 KiB took about 75 ms on a 2.1 GHz core).
4. **Request** – one JSON object in the GraphQL-over-HTTP form (`query`, optional `variables` and
   `operationName`). Batches (JSON arrays) are not accepted.
5. **Bracket scan** (§4.2) of the raw query text, before it is parsed.
6. **Parsing** by the endpoint. A query that does not parse is rejected (`400 VALIDATION`); the engine never
   parses the query again, so nothing that rewrites the query later can bypass the checks.
7. **Document pre-check** (§4.2) on the parsed document, before the GraphQL engine validates it.
8. **Execution** with the engine limits (§4.3), the read limits (§4.4) and the request deadline (default
   10 s): when the deadline passes, the execution is stopped and the answer is `503 TIMEOUT` (§4.5). The
   deadline only takes effect where the execution waits (storage reads, other awaits); it does not stop the
   synchronous work of the engine – parsing, validation, converting argument values. The limits of steps 5
   and 7 are what bound that work.

### 4.2 Document pre-check

Before parsing, one pass over the raw query text bounds the **bracket depth**: GraphQL parsers build nested
values (`[[[…]]]`, `{a: {a: …}}`) recursively, and a few kilobytes of brackets would overflow the stack and
abort the process. The pass counts `[`, `{` and `(` outside strings, block strings and comments and rejects a
query in which more than 64 of them are open at once. Selection sets count too: a selection set at nesting
level *n* (below) is *n* braces deep, so the bracket depth must be at least the nesting limit plus the depth of
the argument values used there (the defaults leave 32 levels for arguments and their values). Unbalanced
brackets are left to the parser. A block string `"""` without a closing `"""` is rejected: a parser may read
its opening quotes as an empty string followed by an ordinary one and parse the text after it, which the pass
would have skipped. (The only documents this refuses are ones like `["""x"]`, a list of `""` and `"x"`.)

The pre-check then runs on the parsed document and costs time linear in its size (each fragment is measured
once), except for the field-merge count, which costs what the engine's own walk costs and stays within the
limits below. It exists because GraphQL engines walk fragment spreads by expanding them: a few fragments that each
spread the next one twice expand to 2^n selections, and the engine's own depth and complexity checks would
block a thread before execution starts. Engines also compare the arguments of fields with the same response
name, quadratic in the number of arguments per field, so the arguments are bounded as well. And they convert
the argument values of every field instance – once per spread of its fragment, copying a variable's value at
every use – so the argument values and the bytes of their texts are counted through the expansion too. Engines repeat names in their error messages, once per error, so names are
short. And the rule for fields with the same response name runs once for every written selection set and walks
the fragments spread there, so a fragment reached through a chain of fragments is walked once per fragment of
the chain – its conflicts are counted the way the engine reports them.

| Limit | Default | What counts |
|---|---|---|
| Bracket depth | 64 | `[`, `{`, `(` open at once in the raw query text, outside strings and comments. |
| Arguments per field or directive | 32 | Arguments written on one field or one directive. |
| Arguments in total | 1 000 | Arguments written in the document (fields and directives, not expanded). |
| Directives per location | 8 | Directives on one operation, variable definition, field, fragment definition, spread or inline fragment. |
| Variable definitions | 50 | Variables declared by one operation. |
| Name length | 256 | Bytes of each operation, field, alias, argument, directive, fragment and variable name (definitions and uses), each type condition and the named type of each variable definition. |
| Fragment definitions | 30 | Fragment definitions in the document. |
| Fragment spreads | 100 | Spreads `...Name` written in the document (not expanded). |
| Expanded selections | 2 000 | Selections (fields, inline fragments, spreads) of all operations together, with every spread replaced by its fragment. |
| Expanded argument values | 10 000 | Value nodes in the arguments of all fields and of the directives on fields, inline fragments and spreads, of all operations together, with every spread expanded. Each scalar, `null`, enum value, list and object counts 1, plus its entries. A variable counts the nodes of its value (from the request – also when no operation declares it, because the engine still validates the request value at every use – else its declared default; the largest over the operations declaring it), because the engine copies the value into every field instance that uses it. The engine also suggests similar field names for unknown fields; that cost grows with the number of fields of the parent type, so apps with very wide types should lower the body limit. |
| Expanded value bytes | 1 MiB (1 048 576) | Bytes of the texts in the same argument values, counted through the same expansion: strings, enum values, binary values and object keys (also the keys of JSON variables); numbers, booleans and `null` count 0. A variable counts the bytes of its value at every use, as for the value nodes, because the engine copies the value into every field instance that uses it. One record-sized variable used once fits; a long string used at hundreds of instances does not. |
| Nesting | 32 | Levels of selection sets, the operation's own set being level 1; every field with a selection set, inline fragment and fragment spread opens a level (as the engine counts its recursion depth). At most 31 levels below the operation. |
| Field-merge conflicts | 20 | For every written selection set (of operations, fields, inline fragments and fragment definitions): its fields together with those of its inline fragments and spreads (each fragment once), grouped by type condition and response name. Each later field of a group adds 1 if its field name differs from the group's first field, 1 if its argument count differs, and 1 for each argument of the first field it does not have with an equal value. The same number as the error limit (§4.5). |
| `_entities` representations | 100 | Sum over all `_entities` fields of all operations. |

Further rules:

- **No fragment cycles.**
- **No unused fragments:** every fragment definition must be reached from an operation through spreads. The
  engine validates unused fragments as well, and they would be walked without counting toward the totals; this
  way every fragment body counts through its spreads.
- Spreads of unknown fragments are left to the engine's validation.
- **Representations.** Every occurrence of `_entities` counts in full: each alias, each inline fragment and each
  spread of a fragment (also the same fragment spread twice), because the engine executes every occurrence
  separately. The value of `representations` is the inline value, or the request variable, or – when the
  request does not send the variable – its declared default value (the engine uses the default too), else
  `null`. A list counts its entries (`[$a, $b]` counts 2), `null` counts 0, any other value 1 (list coercion).

### 4.3 Engine limits

- Query depth and complexity limited by the engine (the app chooses the values; defaults depth 10, complexity
  1 000); directives per location limited as in §4.2.
- **No extensions that rewrite the query** (e.g. persisted queries): the endpoint checks the query it parsed,
  not one an extension substitutes.
- **No introspection:** `__schema` and `__type` answer `null`; `_service { sdl }` stays available for the
  router.
- No mutations and no subscriptions in the subgraph schema.

### 4.4 Reads

- **Read budget per request** (default 1 000): every storage read of the app's resolvers counts. The read
  beyond the budget is not run; its field ends with an error with `extensions.code = "VALIDATION"` and the
  message `Zu viele Lesezugriffe in einer Anfrage.` The answer is `200` with `errors`; the field becomes `null`
  by the GraphQL rules, parts already read stay in `data`.
- **Concurrent reads per process** (default 3): at most this many reads of all subgraph requests of the
  process run at the same time, so subgraph traffic cannot take the connection pool from the app's own users.

### 4.5 Errors

Error bodies have the GraphQL error form and `Cache-Control: no-store`:

```json
{"errors": [{"message": "Anmeldung erforderlich.", "extensions": {"code": "UNAUTHENTICATED"}}]}
```

| Status | `extensions.code` | Message | When |
|---|---|---|---|
| `400` | `BAD_REQUEST` | `Ungültige Anfrage.` | The body is unreadable, not JSON, a batch or not a GraphQL request. |
| `400` | `VALIDATION` | `Ungültige Anfrage.` | The query does not parse, or the document pre-check (§4.2) rejected it. Nothing is executed. |
| `405` | `BAD_REQUEST` | `Ungültige Anfrage.` | Not `POST`. Header `Allow: POST`. |
| `401` | `UNAUTHENTICATED` | `Anmeldung erforderlich.` | Missing or wrong key. Header `WWW-Authenticate: Bearer`. |
| `413` | `VALIDATION` | `Anfrage zu groß.` | The body is larger than the limit. |
| `429` | `RATE_LIMITED` | `Zu viele Fehlversuche. Bitte warten Sie eine Minute.` | Failure brake (§3.2). Header `Retry-After: 60`. |
| `503` | `TIMEOUT` | `Zeitüberschreitung.` | The request deadline (§4.1) passed. Nothing of the partial result is sent. |

The messages name no limit and no detail; the server logs which limit was hit. Everything after the
pre-check is the engine's answer (`200`, with `data` and/or `errors`). An error of the engine itself – one
without a `path`, such as a validation, depth or complexity error – that has no `extensions.code` gets
`extensions.code = "VALIDATION"`; errors of fields (with a `path`) and errors with a code are passed on
unchanged. An answer carries at most 20 errors (the error limit): the first 20 are kept and the rest is replaced
by one final error with `extensions.code = "VALIDATION"` and the message `Weitere Fehler ausgelassen.` This
bounds the answer, not the engine's work – the pre-check does that. A failed storage read of a resolver should end with `extensions.code = "INTERNAL"` and the message
`Interner Fehler. Bitte versuchen Sie es später erneut.`, never with the text of the storage error.
