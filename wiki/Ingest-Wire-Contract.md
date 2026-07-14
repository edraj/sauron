# Ingest Wire Contract

The wire contract is the JSON **envelope** every SDK posts to the ingest gateway. It
is defined authoritatively in
[`backend/crates/sauron-core/src/envelope.rs`](../backend/crates/sauron-core/src/envelope.rs);
a golden fixture in that file and in the SDK test suites guards parity.

See also: **[Home](Home.md)** · **[Getting Started](Getting-Started.md)**.

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
  `sauron-node`, `sauron-python`, `sauron-dotnet`. `header.sdk.version` is `0.1.0`.
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
| `transaction` | ✅ | ✅ | — | — | — |
| `breadcrumb_batch` | ✅ | ✅ | — | — | — |

The server-side SDKs (Node, Python, C#) ship the core dispatch items only — no
auto-instrumentation, no breadcrumb ring, no transactions in v0.1.
