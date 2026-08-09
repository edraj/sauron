# Changelog

## 1.3.0

- **Workflows** — bound a named span of activity with start / end / cancel, and
  read the active one back. Every event, error and transaction captured while a
  workflow is active is stamped with its `workflow_id` / `workflow_name`, so the
  dashboard can group a whole flow (`checkout`, `password_reset`, …) as one unit.
  Entirely optional: an app that never starts a workflow behaves exactly as before.
- **`beforeSend` can no longer throw into your app.** A hook that raises is logged
  and the item is sent unmodified, rather than the exception escaping through the
  capture call. Returning `null` still drops the item as before.
- **`Flush`/`FlushAsync` can no longer throw into your app either.** A failure while
  building or delivering an envelope is logged (with `Debug = true`) instead of
  propagating out of the flush. Nothing is treated as delivered unless it was: an
  envelope whose send failed stays queued for the next flush. Concretely, this also
  fixes `Flush()` after `Close()`, which raised `ObjectDisposedException`, and an
  unserializable property value (e.g. a reference cycle), which raised `JsonException`
  and took the rest of the flush with it.

## 1.2.0

- **Breaking: the `Environment` option has been removed.** An environment is now
  identified by the ingest key it belongs to, not by a string the client sends.
  Create environments in the dashboard under app settings; each one has its own
  DSN. Delete `Environment` from your `SauronOptions` and swap in the DSN of the
  environment you want to report to.
- **Fixed: a `413` head-of-line blocked the whole delivery queue.** The queue is FIFO and a
  payload-too-large envelope was retried forever, so nothing behind it could ever be sent.
  Such an envelope is now dropped with a log line instead.
- **Fixed: oversized envelopes.** The whole buffer became one envelope, which could exceed
  the server's 1000-item limit and be dropped as a non-retryable `400`. Envelopes are now
  capped at `MaxItemsPerEnvelope` (default 1000).

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
