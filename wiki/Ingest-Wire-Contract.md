# Ingest Wire Contract

The wire contract is the JSON **envelope** every SDK posts to the ingest gateway. It
is defined authoritatively in
[`backend/crates/sauron-core/src/envelope.rs`](../backend/crates/sauron-core/src/envelope.rs);
a golden fixture in that file and in the SDK test suites guards parity.

See also: **[Home](Home.md)** · **[Getting Started](Getting-Started.md)** ·
**[Architecture](Architecture.md)** (what the backend does with your envelope).

## DSN

```
https://<public_key>@<host>/<project_id>
```

- `<public_key>` — a **non-secret, write-only** credential (the URL "user" part). Safe
  to embed in client code. A DSN **must not** contain a password/secret component.
- `<host>` — `host:port` of the ingest gateway (`https` or `http`).
- `<project_id>` — the path segment (id/UUID).

## Endpoint

```
POST {protocol}://{host}/api/{project_id}/envelope
```

Headers:

| Header | Value |
| --- | --- |
| `X-Sauron-Key` | `<public_key>` (the DSN normally travels here, not in the body) |
| `Content-Type` | `application/json` |
| `Content-Encoding` | `gzip` (optional) |

A `sendBeacon` fallback (used by the browser SDK) targets the same URL with the key in
the query string: `.../envelope?k=<public_key>`.

### Compression

The body may be gzip-compressed. **Every SDK** (as of **v0.3.0**) gzips the JSON body once
it exceeds a threshold (`gzipThresholdBytes`, default 1024) and sets
`Content-Encoding: gzip`; smaller bodies go out uncompressed and omit the header. The
ingest gateway transparently decompresses when the header is present, so compression is
purely a transport optimization — the decoded JSON is identical either way.

### Responses

- **2xx** — accepted (fire-and-forget).
- **401 / 403** — bad key → the SDK disables and stops retrying.
- **429 / 5xx** — transient → bounded retry with backoff, or drop.

## Envelope

One envelope carries a **header**, an envelope-wide **context** block, and a list of
tagged **items**. Almost every field has a server-side default; only
`header.sdk.{name,version}` is strictly required, and `sent_at` / `timestamp` fields
default to now.

```json
{
  "header": {
    "dsn": "https://pk_test@localhost:8081/1",
    "sdk": { "name": "sauron.javascript", "version": "0.1.0" },
    "sent_at": "2026-07-12T10:30:00.123Z",
    "environment": "production",
    "release": "web@1.4.2"
  },
  "context": {
    "device":  { "family": "Apple", "model": null, "arch": null },
    "os":      { "name": "macOS", "version": "14.5" },
    "app":     { "version": "1.4.2", "build": null },
    "runtime": { "name": "Chrome", "version": "126" },
    "user":    { "id": "u_123", "email": null, "traits": {} }
  },
  "items": [ /* tagged items — see below */ ]
}
```

- **`header.sdk.name`** identifies the emitting SDK: `sauron.javascript` (browser),
  `sauron-node`, `sauron-python`, `sauron-dotnet`. `header.sdk.version` is the SDK's own
  release — all five ship as `0.3.0` (the `0.1.0` in the example above mirrors the golden
  parity fixture).
- **`context`** blocks (`device`, `os`, `app`, `runtime`) are free-form JSON so SDKs
  stay unopinionated about platform fields. Only `user` is typed (id / email /
  username / ip_address / traits) because the backend resolves it to an identity.

## Items

Each item is tagged by a `type` discriminant (`snake_case`).

### `event` — a `track()` product-analytics event

```json
{ "type": "event", "name": "checkout_completed", "distinct_id": "u_123",
  "properties": { "cart_value": 42.5 }, "timestamp": "2026-07-12T10:29:40.000Z",
  "session_id": null, "screen": null }
```

`name` and `distinct_id` are **required**.

### `error` — a captured exception or message

```json
{ "type": "error", "event_id": "<uuid>", "level": "error",
  "timestamp": "2026-07-12T10:29:58.900Z",
  "exception": {
    "type": "TypeError", "value": "x is not a function",
    "mechanism": { "type": "onunhandledrejection", "handled": false },
    "stacktrace": [
      { "function": "loadUser", "module": null, "filename": "app.js",
        "abs_path": null, "lineno": 42, "colno": 13, "in_app": true }
    ]
  },
  "message": null,
  "breadcrumbs": [
    { "type": "navigation", "category": "history", "message": null, "level": "info",
      "timestamp": "2026-07-12T10:29:50.000Z", "data": { "from": "/", "to": "/settings" } }
  ],
  "tags": {}, "fingerprint": null, "user": null,
  "session_id": null, "screen": null }
```

- `level` ∈ `debug | info | warning | error | fatal` (default `error`).
- **Stack frames are ordered call-site → crash — the crashing frame is LAST.**
- `fingerprint` (optional) overrides server-side grouping when present.
- **`breadcrumbs`, `tags`, `user`, and `fingerprint` are populated by every SDK** (as of
  **v0.3.0**) from the active [scope](Capabilities.md): the SDK merges the scope's recent
  breadcrumb ring, its tags, and its user onto each captured error, and honors a
  client-supplied `fingerprint` verbatim. All four are optional — omitted keys fall back to
  server-side defaults (empty ring, `{}` tags, the envelope `context.user`, backend
  grouping).

### `identify` — attach traits to a person

```json
{ "type": "identify", "distinct_id": "u_123", "anonymous_id": null,
  "traits": { "plan": "pro" }, "timestamp": "<iso8601>" }
```

`distinct_id` is **required**. `anonymous_id` lets you alias an anonymous id onto a
known one.

### `transaction` — a performance span

```json
{ "type": "transaction", "name": "GET /api/users", "op": "http",
  "duration_ms": 128.4, "status": "ok", "http_method": "GET", "http_status": 200,
  "url": "/api/users", "distinct_id": "u1", "session_id": "s1",
  "timestamp": "<iso8601>" }
```

`name`, `op`, and `duration_ms` are **required**. `op` ∈
`navigation | http | resource | screen_load | custom`. Aggregated server-side into
p50/p95/etc.

**Every SDK emits transactions** via `trackTransaction` (as of **v0.3.0**) — the browser
and Flutter can also auto-capture navigation/HTTP/route timings, while the server SDKs
(Node, Python, C#) record them manually (e.g. per request handler). Wire fields are
snake_case (`duration_ms`, `http_method`, `http_status`, `distinct_id`).

### `breadcrumb_batch` — a standalone trail of breadcrumbs

```json
{ "type": "breadcrumb_batch", "distinct_id": "u1", "session_id": "s1",
  "breadcrumbs": [ { "type": "navigation", "timestamp": "<iso8601>", "data": {} } ] }
```

Uploaded ahead of (or alongside) an error so the backend can attach recent activity to
a later crash for the same person.

## Which SDK emits which items

| Item | Browser | Flutter | Node | Python | C# |
| --- | :-: | :-: | :-: | :-: | :-: |
| `event` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `error` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `identify` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `transaction` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `breadcrumb_batch` | ✅ | ✅ | — | — | — |

As of **v0.3.0** the server-side SDKs (Node, Python, C#) reach parity on the dispatch
surface: they emit `transaction` items and carry a breadcrumb ring, tags, and user on
their `error` items. They still do **not** upload a standalone `breadcrumb_batch` — the
browser and Flutter use that to pre-stage a person's recent activity, whereas the server
SDKs attach breadcrumbs directly onto the error item. See the full
[Capabilities](Capabilities.md) matrix.
