# Changelog

All notable changes to the Sauron Python SDK are documented here.

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
