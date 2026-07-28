# Changelog

All notable changes to `@edraj/sauron-node` are documented here.

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
