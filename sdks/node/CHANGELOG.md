# Changelog

All notable changes to `@edraj/sauron-node` are documented here.

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

  Signature: `trackTransaction({ …, tags?: Record<string, string>, extra?: Record<string, unknown> })`. `MAX_TRANSACTION_EXTRA_BYTES` and `capTransactionExtra` are now exported from the package entrypoint, so a caller can size a payload before attaching it.

## 1.4.0

### Fixed

- **`trackTransaction` shipped transactions with no duration, silently.** This
  SDK's input field is `duration_ms`; the browser SDK's is `durationMs`. A
  snippet moved between the two — or any plain-JavaScript caller, where the type
  checker is not there to object — produced a transaction item missing the one
  field it exists to carry, and nothing anywhere complained: the item validated,
  shipped, and looked delivered.

  `durationMs` is now accepted as an alias, and a transaction with no usable
  duration is **dropped with a debug log line rather than sent**. Refusing is the
  point: sending it anyway is what let the misspelling survive unnoticed.
  `duration_ms: 0` is a legitimate duration and is still sent.

### Changed

- `TransactionInput.duration_ms` is now optional, since `durationMs` may supply
  it instead. Supply exactly one — `duration_ms` wins if both are present.
  Supplying neither is the drop above, not a compile error.

## 1.3.0

### Added

- **Workflows** — bound a named span of activity with `startWorkflow(name,
  { force? })` / `endWorkflow(name?)` / `cancelWorkflow(name?, { reason? })`,
  and read the active one with `getWorkflow()`. Every event, error and
  transaction captured while a workflow is active is stamped with its
  `workflow_id` / `workflow_name`, so the dashboard can group a whole flow
  (`checkout`, `password_reset`, …) as one unit. The three lifecycle events
  `$workflow_start` / `$workflow_end` / `$workflow_cancel` are emitted
  automatically, carrying `duration_ms` (and `reason` on cancel).
- New exported types: `WorkflowStatus`, `WorkflowResult`, `ActiveWorkflow`.
- `SauronClient.isEnabled()` — `false` once the transport has auto-disabled
  itself on a 401/403.

Workflows are entirely **optional**: an app that never calls them emits
byte-identical telemetry to 1.2.0 — the two fields are omitted from the wire
JSON, never sent as `null`.

The active workflow is **request-scoped**, held on the current `Scope` and
isolated by the same `AsyncLocalStorage` that already isolates
`user`/`tags`/`breadcrumbs`. Concurrent requests never observe or clobber each
other's workflow. None of the three methods ever throws — each returns one of
six statuses (`ok`, `already_active`, `not_active`, `name_mismatch`,
`invalid_name`, `disabled`).

## 1.2.0

- **Breaking: the `environment` option has been removed.** An environment is now
  identified by the ingest key it belongs to, not by a string the client sends.
  Create environments in the dashboard under app settings; each one has its own
  DSN. Delete `environment` from your `init` call and swap in the DSN of the
  environment you want to report to.

## 1.0.0 - 2026-07-27

First public release. Prior `0.x` versions were internal-only and were never
published to npm.

- **Renamed to `@edraj/sauron-node`.** The wire identity is unchanged — the SDK
  still reports itself as `sauron-node` in the envelope header — so the rename
  is invisible to the ingest gateway and the dashboard.
- **Fixed: a full send queue could wedge the transport permanently.** The whole queue went
  out as one envelope and `413` was treated as retryable, so once the buffer filled during
  an outage every flush resent the same oversized body and failed identically — no event was
  ever delivered again. A 413 now halves the envelope and re-buffers instead of retrying
  unchanged, and a single item that still will not fit is dropped rather than looping.
- **Fixed: an oversized envelope could delete the entire offline backlog.** Exceeding the
  server's 1000-item limit is a non-retryable `400`, which committed the batch and unlinked
  every persisted file. Envelopes are now capped at `maxItemsPerEnvelope` (default 1000) and
  the queue drains in chunks.

## 0.3.0

Parity release — brings the server SDK up to the Browser/Flutter feature bar and
reconciles the wire shape against `backend/crates/sauron-core/src/envelope.rs`.

### Added

- **Scope + per-request isolation** via `AsyncLocalStorage`: `withScope`,
  `configureScope`, `setUser`, `setTag`, `setTags`, `setContext`, `setExtra`.
  Concurrent requests no longer leak user/tags/breadcrumbs into each other.
- **Breadcrumbs**: `addBreadcrumb` on the active scope (bounded ring buffer,
  default 100) with an optional `beforeBreadcrumb(crumb)` hook. Captured errors
  now attach the scope's breadcrumb trail (previously always `[]`).
- **`beforeSend(item, hint?)`** hook running on every outgoing item
  (`error | event | identify | transaction`); return `null` to drop.
- **`trackTransaction(input)`** — manual performance transactions
  (`envelope.rs::TransactionItem`), with `distinct_id` falling back to the
  scoped user's id.
- **Opt-in auto-capture** (`autoCaptureUnhandled`, default off): captures
  `uncaughtException` / `unhandledRejection` with `mechanism.handled = false`.
  Never swallows the crash — Node's default exit is preserved when the SDK is
  the sole handler.
- **Opt-in graceful shutdown** (`autoShutdown`, default off) plus the exported
  `installShutdownHooks(client)` / `installAutoCapture(client)` helpers wiring
  `beforeExit`/`SIGTERM`/`SIGINT` to `close()`.
- **Gzip transport**: request bodies over `gzipThresholdBytes` (default 1024)
  are gzipped with `Content-Encoding: gzip`.
- **Retry/backoff policy**: exponential backoff + jitter on 408/413/429/5xx and
  network errors, honoring `Retry-After`; drop (no retry) on 400/401/403/404.
- **Bounded send queue** (`maxQueueBytes`, default 1 MiB, drop-oldest) with
  **opt-in disk persistence** (`offlineDir`) for at-least-once delivery across
  restarts.

### Changed

- Error items reconciled to the canonical `envelope.rs::ErrorItem` field set:
  real `breadcrumbs`, `tags`, `user` from scope plus an optional `fingerprint`
  override. Guarded by a new golden-envelope fixture test (`test/envelope.test.ts`).
- SDK version reported on the wire header bumped to `0.3.0`.

## 0.1.0

- Initial server-side SDK: `init`, `track`, `captureException`,
  `captureMessage`, `identify`, buffered background transport.
