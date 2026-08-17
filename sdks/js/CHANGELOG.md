# Changelog

All notable changes to `@edraj/sauron-browser` are documented here.

## 1.6.0

### Added

- **Navigation direction on history breadcrumbs.** SPA navigation breadcrumbs
  now carry `operation` alongside `from` and `to`: `push` for
  `history.pushState`, `replace` for `history.replaceState`, and `pop` for
  `popstate`. A breadcrumb trail no longer reads the same whether the user
  advanced through a flow or backed out of it.

  The vocabulary is shared with the Flutter SDK's `SauronNavigatorObserver`
  (`push` / `pop` / `replace` / `remove`), so a trail reads the same whichever
  SDK sent it. `remove` has no web equivalent and is never emitted here.

  **A forward navigation is recorded as `pop`.** `history.forward()` fires the
  same `popstate` event as `history.back()` and carries nothing to separate
  them, so `pop` means "moved through history" rather than specifically "went
  back". Telling them apart would mean writing a counter into `history.state`,
  which the host app's router also owns — not a trade this SDK makes to
  improve a breadcrumb.

  No API change: `operation` is added to `data` on breadcrumbs the SDK already
  emitted, and the same-path guard still suppresses a `replaceState` to the
  current URL before any direction is recorded.

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

## 1.4.1

### Added

- **Auto-reset on identity switch.** `identify()` now detects a login by a
  DIFFERENT user than last time on the same device — the common case of a
  forgotten `reset()` on logout — and mints a fresh anonymous id (and rotates
  the session id) before sending, so `anonymous_id` is `null` instead of an
  alias to the previous person. This can't undo an alias already sent under
  the old id — still call `reset()` on logout — but it bounds a missed
  `reset()` to one corrupted guest window instead of every one after it. To
  detect the switch, `identify()` persists a short one-way digest (never the
  id itself; see `hashIdentity`) of the last identified user in `localStorage`
  under `sauron.last_identified`. Like the anonymous id, this is a durable
  first-party value stored on the user's terminal — a retention and consent
  consequence, not just an implementation detail.

  The stored value carries a format tag: `v1:<digest>`, byte-identical to what
  the Flutter SDK writes under the same key. A value with no tag or an
  unrecognised one reads as "nobody has identified on this device yet" and is
  rewritten in the current format on the next `identify()`. That matters
  because the digest's shape is not frozen — if it ever widens again, an
  untagged store could not tell "a digest I no longer produce" from "a
  different person", so every returning user's next `identify()` would be read
  as a switch and would rotate their anonymous id and session, once, silently.
  The tag turns that into one missed switch per device instead.

### Changed

- `reset()` now also rotates the session id (`sauron.session_id`). The
  server's `bump_session` is last-write-wins on `distinct_id`, so without
  this a single `sessions` row could otherwise end up serially representing
  two different people and recording only whichever wrote last.

## 1.4.0

### Fixed

- **`captureMessage()` poisoned the entire envelope.** It sent an `exception`
  block with `type: null`, but the gateway's `ExceptionInfo.ty` is a
  non-optional string with no default, so the envelope failed to deserialize and
  was rejected whole (`400 invalid_envelope`) — taking every unrelated error,
  event and transaction batched alongside it. Every SDK treats a 400 as
  non-retryable, so the batch was dropped without a retry and without a trace.
  A message item now carries no `exception` block at all and puts its text in
  `message`, which is the shape the Python SDK already sent.

  Worth knowing before you upgrade: this also changes server-side grouping.
  Messages now fingerprint on their normalized text instead of piling into one
  bucket keyed by a synthetic exception type, so existing message issues will
  re-split into separate issues.
- **A failed offline-queue drain silently deleted the rest of the backlog.**
  `drain()` empties `localStorage` in one shot, so from that point the parked
  envelopes exist only in a local array — and a send failure re-parked just the
  payload that had failed before returning, discarding every payload behind it.
  The whole untried remainder is now re-parked, at the head, preserving the
  oldest-first order the byte-cap eviction policy depends on. This is the plain
  reconnect-then-one-500 case the queue exists for. A 401/403 mid-drain now
  keeps the backlog too — the credentials may be fixed and the client
  re-inited — and logs a warning instead of disabling in silence.

### Changed

- The anonymous id is now persisted in `localStorage` under `sauron.anon_id`
  instead of being re-minted in memory on every page load. **Every web app's
  reported active-user count drops sharply and permanently on the day this is
  adopted** — the old behaviour counted page loads, not people (a 5-10x
  inflation, all of it in the "guest" half of the Active Users report). The
  drop is a data artifact, not a regression.
- The anonymous id is a durable first-party identifier stored on the user's
  terminal. That is a retention and consent consequence, not just an
  implementation detail.
- `ExceptionValue.type` is `string`, no longer `string | null`, and `ErrorItem`
  gained an optional `message`. A TypeScript caller that builds these by hand
  will now see a type error where it previously compiled — passing `null` there
  is precisely what produced the envelope rejection above.

### Added

- `reset()` — clears the scope user and mints a fresh anonymous id.
  **Call it on logout.** `setUser(null)` now calls it for you. Without it, the
  next anonymous visitor on a shared browser reuses the persisted id and a
  later `identify()` aliases their activity to the previous account,
  server-side, permanently.
- `anonymous_id` is sent on the identify item only when the anonymous id was
  actually used as a `distinct_id` in this browser session.

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
published to npm.

### Package

- **Renamed to `@edraj/sauron-browser`.** The wire identity is unchanged — the
  SDK still reports itself as `sauron.javascript` in the envelope header — so
  the rename is invisible to the ingest gateway and the dashboard.

### Capture

- Automatic capture of uncaught errors and unhandled promise rejections.
- `captureException()` / `captureMessage()` for manual reporting.
- Stack-trace parsing with in-app frame detection, and `debug_id` extraction
  for source-map symbolication on the server.

### Product analytics

- `track()`, `identify()`, `trackTransaction()`.
- Screen tracking via `setScreen()` / `getScreen()`.
- Opt-in automatic capture (clicks, navigation, page views).

### Scope and metadata

- `setUser()`, `setTag()`, `setTags()`, `setContext()`, `setExtra()`.
- Breadcrumbs via `addBreadcrumb()`, plus automatic console/navigation/fetch
  breadcrumbs.
- `beforeSend` hook runs on **every** item, not just errors, so analytics
  events can be scrubbed or dropped the same way errors can.

### Transport

- Batches items, gzips them with `fflate`, and delivers envelopes to the
  ingest gateway.
- Honors the full ingest response policy, including rate-limit backoff and
  `401` shutdown.
- Envelopes are chunked to at most `maxItemsPerEnvelope` (default 1000) so a
  backlog can never build a body the server rejects as non-retryable.
- `flush()` / `close()` for deterministic shutdown.
