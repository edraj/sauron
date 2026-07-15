# Changelog

## 0.3.0

Parity release — the .NET SDK reaches the Browser/Flutter feature bar and converges on
the canonical ingest wire shape (`backend/crates/sauron-core/src/envelope.rs`).

### Added

- **Scope + per-request isolation** (`AsyncLocal`): `SetUser`, `SetTag`, `SetTags`,
  `SetContext`, `SetExtra`, and `using (SauronSdk.PushScope())` for isolated per-request
  scopes.
- **Breadcrumbs**: `AddBreadcrumb` on the active scope with a bounded ring
  (`MaxBreadcrumbs`, default 100) and a `BeforeBreadcrumb` hook. Captured errors now carry
  the scope's breadcrumb trail.
- **`BeforeSend`**: runs on every outgoing item (event, error, identify, transaction);
  return `null` to drop, or a replacement to mutate.
- **Transactions**: `TrackTransaction(name, durationMs, op, ...)` emits a `transaction`
  item; `distinctId` falls back to the scoped user id.
- **Gzip transport**: request bodies over `GzipThresholdBytes` (default 1024) are gzipped
  with `Content-Encoding: gzip`.
- **Retry/backoff policy**: retry 408/413/429/5xx + network errors with exponential
  backoff + jitter (cap 30s), honor `Retry-After` on 429, drop on 400/401/403/404.
- **Bounded queue + opt-in disk persistence**: byte-capped in-memory ring
  (`MaxQueueBytes`, default 1 MiB) with optional FIFO on-disk persistence (`OfflineDir`).
- **Opt-in auto uncaught-error capture** (`AutoCaptureUnhandled`, default off): wires
  `AppDomain.UnhandledException` and `TaskScheduler.UnobservedTaskException`, capturing
  with `mechanism.handled = false` while preserving the runtime's default crash/exit
  behavior.
- **Fingerprint override**: optional `fingerprint` argument on `CaptureException` /
  `CaptureMessage`, honored verbatim by the backend for grouping.

### Changed

- Error items now emit the reconciled canonical field set (`event_id`, `level`,
  `timestamp`, `exception`, `message`, `breadcrumbs`, `tags`, `fingerprint`, `user`).
  `fingerprint` is now an array of strings (`Vec<String>` on the wire) rather than a
  single string.
- SDK header version bumped to `0.3.0`.

### Testing

- Added a golden-envelope fixture test (`EnvelopeGoldenTests`) asserting byte/shape
  parity with the locked wire contract, plus opt-in auto-capture tests.
