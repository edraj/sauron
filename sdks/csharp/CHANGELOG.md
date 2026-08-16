# Changelog

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

  Signature: `TrackTransaction(..., IReadOnlyDictionary<string, object?>? tags = null, IReadOnlyDictionary<string, object?>? extra = null)`. `TransactionExtra.MaxBytes` is public so a caller can size a payload before attaching it.

## 1.4.0

- **`Flush`/`FlushAsync` can no longer throw into your app.** A failure while
  building or delivering an envelope is logged (with `Debug = true`) instead of
  propagating out of the flush. Nothing is treated as delivered unless it was: an
  envelope whose send failed stays queued for the next flush. Concretely, this also
  fixes `Flush()` after `Close()`, which raised `ObjectDisposedException`, and an
  unserializable property value (e.g. a reference cycle), which raised `JsonException`
  and took the rest of the flush with it.
- **Fixed: `Close()` could hang a concurrent flush forever.** `Dispose()` disposed the
  drain lock, and `SemaphoreSlim.Dispose()` *abandons* already-queued async waiters —
  it neither completes nor faults them — so a timer- or `Enqueue`-driven flush already
  awaiting the lock never resumed, and the caller's `await FlushAsync()` never
  returned. The lock is no longer disposed; a `SemaphoreSlim` holds no unmanaged
  resource unless `AvailableWaitHandle` is read, which this SDK never does.
- **Fixed: one unsendable envelope stopped the whole drain.** Failures are now
  classified. An envelope-local failure leaves that envelope queued and moves on to
  the next one, so it cannot head-of-line block everything behind it; a
  teardown-class failure (cancellation, a disposed dependency, out-of-memory) ends
  the pass with the queue intact. Neither path acks an envelope it did not deliver.
- **Fixed: an unserializable item took the rest of the flush with it.** Serialization
  runs over caller-supplied properties, tags and extra, so a reference cycle or a
  throwing property getter aborted the flush and stranded every well-formed item
  behind it. That chunk is now dropped with a log line and the flush continues.
- Debug log lines for unexpected failures now include the exception type, not just
  its message.

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
