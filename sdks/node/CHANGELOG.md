# Changelog

All notable changes to `@sauron/node` are documented here.

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
