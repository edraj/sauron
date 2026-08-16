# Changelog

All notable changes to the Sauron Python SDK are documented here.

## 1.5.0

### Added

- **`tags` and `extra` on transactions.** `trackTransaction` now accepts two
  developer-supplied maps: `tags` (flat string→string, indexed) and `extra`
  (freeform JSON). `extra` is where a request body, a response body, an order
  id or a retry count goes — the span that times an HTTP call can now carry
  what the call actually sent and received.

  Both are visible on the new **Transactions** page and in the session
  timeline, and both are searchable: `@tag.tier:premium`,
  `extra.order_id:9001`, or `extra.response:~9001` to match a substring
  *inside* a stored response body.

  **They are per-call only.** Unlike `captureException` and `track`, a
  transaction does not inherit the scope — `setTag()` / `setExtra()` defaults
  are not merged in. Transactions are the highest-volume signal an app emits,
  one per navigation and per request, so inheriting a global blob would write
  it onto every row. This asymmetry is deliberate and is documented on the
  method.

  `extra` is serialized and capped at **16 KB**. Past that the whole map is
  replaced with a `{"_truncated": true, "_bytes": N}` marker, and the
  dashboard says so on the row rather than showing a short object that looks
  complete. The cap is not cosmetic: envelopes are batched, and one oversized
  body would push the whole envelope past the ingest limit and take every
  unrelated span sent with it — a silent loss of data nobody asked about.
  Size is measured in UTF-8 bytes, so non-ASCII payloads are counted at what
  they actually cost on the wire.

  Nothing in `extra` is scrubbed. `beforeSend` remains the redaction seam;
  think twice before attaching a body that can carry tokens or personal data.

  An app that sets neither field serializes byte-identically to before: both
  keys are omitted when empty, never sent as `null`.

  Signature: `track_transaction(..., tags: Optional[Dict[str, str]] = None, extra: Optional[Dict[str, Any]] = None)`. A payload that cannot be JSON-encoded becomes the truncation marker with `"_bytes": -1` rather than raising out of `track_transaction`.

## 1.4.0

**No functional changes.** The `sauron` package is identical to 1.3.0 apart from
`SDK_VERSION`, which is reported in the envelope header's `sdk` block. The
version moved because the JS, Node and .NET SDKs moved; nothing in this
package's behaviour differs from 1.3.0.

- Added a cross-SDK wire-fixture conformance suite (`tests/test_wire_fixture.py`)
  that checks this SDK's envelope against the shared fixtures in
  `sdks/wire-fixtures/`, so a wire-shape drift between the five SDKs fails a test
  here instead of at the ingest gateway. Test-only — not shipped in the wheel.

## 1.3.0

- **Workflows** — bound a named span of activity with start / end / cancel, and
  read the active one back. Every event, error and transaction captured while a
  workflow is active is stamped with its `workflow_id` / `workflow_name`, so the
  dashboard can group a whole flow (`checkout`, `password_reset`, …) as one unit.
  Entirely optional: an app that never starts a workflow behaves exactly as before.
- **`beforeSend` can no longer throw into your app.** A hook that raises is logged
  and the item is sent unmodified, rather than the exception escaping through the
  capture call. Returning `null` still drops the item as before.

## 1.2.0

- **Breaking: the `environment` option has been removed.** An environment is now
  identified by the ingest key it belongs to, not by a string the client sends.
  Create environments in the dashboard under app settings; each one has its own
  DSN. Delete `environment` from your `init` call and swap in the DSN of the
  environment you want to report to.

## 1.0.0 - 2026-07-27

First public release. Prior `0.x` versions were internal-only and were never
published to PyPI.

- **Fixed: a rejected envelope was replayed from disk forever.** Persisted files were only
  deleted on success, so a payload the server permanently refuses (e.g. a `400`) was reloaded
  and re-sent on every process start. Rejections now delete their persisted copies; transient
  failures still keep them for retry.
- **Fixed: oversized envelopes.** The whole queue went out as one envelope, so a recovered
  backlog could exceed the server's 1000-item limit and be dropped as a non-retryable `400`.
  Envelopes are now capped at `MAX_ITEMS_PER_ENVELOPE` (1000) and the queue drains in chunks.
- `413` is no longer retried unchanged — the envelope is split in half and each half sent.

## 0.3.0

The **parity release** — brings the Python SDK up to the Browser/Flutter feature
bar and reconciles the emitted wire shape with the canonical contract in
`backend/crates/sauron-core/src/envelope.rs`. Stdlib only; no new runtime deps.

### Added

- **Scope + per-request isolation** built on `contextvars`: `set_user`,
  `set_tag`, `set_tags`, `set_context`, `set_extra`, `configure_scope`, and the
  `scope()` context manager (plus `push_scope`/`pop_scope`). Concurrent requests
  no longer leak each other's user/tags/breadcrumbs.
- **Breadcrumbs**: `add_breadcrumb(...)` on the active scope (bounded ring,
  `max_breadcrumbs` default 100) with an optional `before_breadcrumb` hook.
  Captured errors now attach the scope's breadcrumb trail.
- **`before_send(item, hint)`** hook — runs on **every** outgoing item
  (error/event/identify/transaction); return `None` to drop.
- **`track_transaction(...)`** — manual performance transactions
  (`envelope.rs::TransactionItem`); `distinct_id` falls back to the scoped user.
- **Gzip** request compression over `gzip_threshold_bytes` (default 1024) with
  `Content-Encoding: gzip`.
- **Retry policy** aligned to the shared table (retry 408/413/429/5xx + network,
  honor `Retry-After` on 429, drop on 400/401/403/404, cap 30s).
- **Bounded in-memory queue** (`max_queue_bytes`, drop-oldest) with opt-in FIFO
  disk persistence via `offline_path` (reloaded on init, deleted on delivery).
- **Opt-in auto uncaught-error capture** — `init(auto_capture_unhandled=True)`
  installs `sys.excepthook` (and `threading.excepthook`) that capture with
  `mechanism.handled=false`, then delegate to the previous hook so the default
  crash/exit behavior is preserved. Off by default.
- **Graceful shutdown** — `init` registers an `atexit` flush (idempotent);
  `flush()` / `close()` remain available.
- **Fingerprint override** — `capture_exception(..., fingerprint=[...])` honored
  verbatim by the backend for custom grouping.
- **Golden-envelope fixture test** guarding byte/shape parity with the shared
  golden (server error item with breadcrumbs+tags+user+fingerprint, an event, an
  identify, and a transaction).

### Changed

- `SDK_VERSION` and the package version bumped to **0.3.0**.

## 0.1.0

- Initial server-side SDK: `init`, `track`, `identify`, `capture_exception`,
  `capture_message`, buffered background `urllib` transport, DSN parsing.
