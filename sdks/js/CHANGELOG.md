# Changelog

All notable changes to `@edraj/sauron-browser` are documented here.

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
