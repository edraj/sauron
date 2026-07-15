# Full SDK Parity + Wiki — Design

**Date:** 2026-07-15
**Status:** Approved (design), not yet implemented
**Scope decision:** "Full parity, all 5" — bring Node/Python/C# up to the Browser/Flutter
feature bar, reconcile the error-envelope shape across all five SDKs, and grow the wiki
to match. **SDK code + wiki only — no backend/DB/migration/pipeline changes.**

---

## 1. Goal

Make all five client SDKs — `@sauron/browser` (js), `sauron_flutter` (flutter),
`@sauron/node`, `sauron` (python), `Sauron` (csharp) — usable *effectively* by
consumers: a consistent, idiomatic, production-grade surface across every language,
emitting an identical wire shape.

Today the two client SDKs (browser, Flutter — both v0.2.0) are full-featured, while the
three server SDKs (Node/Python/C#, all v0.1.0) are a minimal init/track/capture core
with no scope, no breadcrumbs, no before-send, no auto-capture, and inconsistent
transport reliability. This design closes that gap.

## 2. Key finding — the backend already supports everything

The canonical wire contract lives in `backend/crates/sauron-core/src/envelope.rs`. It is
a **tolerant superset**: every field is `#[serde(default)]`, and it already models
`ErrorItem { event_id, level, timestamp, exception, message, breadcrumbs, tags,
fingerprint, user, session_id, screen, raw_stacktrace, debug_meta }`, plus
`TransactionItem`, `BreadcrumbBatch`, and (per the module docs, line 9)
`Content-Encoding: gzip`.

Therefore **nothing in this initiative touches the backend, the DB, the pipeline, or the
ingest**. The reconciliation target is simply this file's shape; every SDK change is
additive and already accepted by the running ingest.

## 3. Approach

**Shared capability model, idiomatic per language.** One capability spec (below),
implemented in each language's native idiom rather than a literal port of the browser
SDK. Each SDK keeps its own transport but converges on identical wire output, guarded by
a **golden-envelope fixture test** in each SDK (the pattern that already exists in
`sdks/js/test/envelope.test.ts`, `sdks/flutter/test/envelope_test.dart`,
`sdks/python/tests/test_envelope.py`, and the Rust `GOLDEN` in `envelope.rs`).

Idiom mapping for cross-cutting mechanisms:

| Mechanism | Node | Python | C# |
| --- | --- | --- | --- |
| Per-request scope isolation | `AsyncLocalStorage` | `contextvars.ContextVar` | `AsyncLocal<T>` |
| Scoped block | `withScope(cb)` | `with sauron.scope():` / `push_scope()` | `using SauronSdk.PushScope()` |
| Auto-flush on exit | `process.on('beforeExit'/SIGTERM/SIGINT)` | `atexit` | `Close()`/`Dispose()` (already) |
| Gzip | `zlib.gzipSync` | `gzip` module | `GZipStream` |

## 4. Capability model

The unified surface. **Client SDKs (browser/Flutter) already have all of this** except
the reconciliation items in §5; the work below is primarily for the three server SDKs.

### 4.1 Scope — global + per-request isolation

The single most important server ergonomics gain. A naïve global mutable scope leaks one
request's user/tags into a concurrent request. Model:

- **Global scope** — process-wide defaults set once at/after init: `release`,
  `environment`, global tags, global context, and a fallback user.
- **Isolated scope** — a per-request/per-task scope layered over the global one via the
  language's async-local primitive. Reads merge child-over-parent.

APIs (names per language idiom):

- Node: `configureScope(cb)`, `withScope(cb)`, `setUser(user|null)`, `setTag(k,v)`,
  `setTags({})`, `setContext(key, obj)`, `setExtra(k,v)`.
- Python: `sauron.set_user(...)`, `set_tag`, `set_tags`, `set_context`, `set_extra`;
  `with sauron.scope() as s:` (auto-pop) and `push_scope()`/`pop_scope()`.
- C#: `SauronSdk.SetUser`, `SetTag`, `SetTags`, `SetContext`, `SetExtra`;
  `using (SauronSdk.PushScope()) { ... }`.

`track()` gains an **optional** `distinctId`/`distinct_id` that falls back to the scoped
user's id when omitted (explicit id still supported and still required if no scoped user
exists — the wire contract mandates a `distinct_id` on analytics events). `captureX`
reads scoped user/tags/breadcrumbs automatically; per-call `user`/`tags` still override.

### 4.2 Breadcrumbs

- `addBreadcrumb(...)` on the active scope; bounded ring buffer (`maxBreadcrumbs`,
  default 100 server / existing 50 js / 100 flutter — align servers to 100).
- Captured errors attach the scope's breadcrumb trail (fills the field currently
  hardcoded `[]` in node `client.ts:135`, python `_client.py:158`, and absent in C#).
- Optional `beforeBreadcrumb(crumb) -> crumb|null` hook.
- Breadcrumb shape matches `envelope.rs::Breadcrumb`: `{ type, category, message, level,
  timestamp, data }`.

### 4.3 before-send / before-breadcrumb hooks

- `beforeSend(item, hint?) -> item|null` — runs on **every** outgoing `EnvelopeItem`
  (error, event, identify, transaction), return `null` to drop. The PII-scrubbing /
  redaction seam backend deployments need.
- `beforeBreadcrumb(crumb, hint?) -> crumb|null`.
- Semantics match the browser SDK (`sdks/js`), which already scopes `beforeSend` to any
  item.

### 4.4 Auto uncaught-error capture — **opt-in, off by default**

Init option `autoCaptureUnhandled` (default `false`). When on:

- Node: `process.on('uncaughtException', ...)` + `process.on('unhandledRejection', ...)`,
  captured with `mechanism.handled = false`; re-throws / preserves Node's default exit
  behavior after flushing.
- Python: installs `sys.excepthook` (chains the previous hook); optional
  `threading.excepthook` and asyncio exception handler.
- C#: `AppDomain.CurrentDomain.UnhandledException` +
  `TaskScheduler.UnobservedTaskException`.

Off by default because uncaught handlers that swallow or alter exit behavior are risky in
a server; consumers opt in explicitly. Mirrors how the browser SDK gates `performance`.

### 4.5 Transactions / performance

- Manual `trackTransaction({ name, op, duration_ms, status?, http_method?, http_status?,
  url?, distinct_id? })` on all three server SDKs (mirrors Flutter's manual API and emits
  `envelope.rs::TransactionItem`). `op ∈ navigation|http|resource|screen_load|custom`.
- Auto HTTP-server timing is delivered as **wiki framework-middleware recipes**, not baked
  into the SDK core (keeps the core unopinionated; see §6).

### 4.6 Gzip compression

- Gzip the request body when it exceeds a threshold (`gzipThresholdBytes`, default
  `1024`, matching js/flutter) and set `Content-Encoding: gzip`. The ingest already
  accepts it.

### 4.7 Retry / backoff parity

- Node currently **drops the batch on any transient failure** (`node/src/transport.ts`).
  Add exponential backoff + jitter, honoring `Retry-After` on 429, retrying 408/413/429/
  5xx, dropping on 400. Python (`_transport.py`) and C# (`Transport.cs`) already retry —
  align their policy to the same table so all three behave identically.

### 4.8 Bounded queue + opt-in disk persistence

- Default: **in-memory bounded ring** (drop-oldest, byte-capped via `maxQueueBytes`) —
  the practical server "offline" story (survive a transient ingest outage without
  unbounded memory growth).
- **Opt-in disk persistence** via a configurable directory (`offlineDir` / `offline_path`
  / `OfflineDir`), default off. When set, envelopes are persisted FIFO and drained on
  next start for at-least-once delivery across restarts. Off by default because servers
  shouldn't be forced into disk I/O / filesystem assumptions (ephemeral containers).

### 4.9 Graceful shutdown

- Python: register `atexit` to flush+close (idempotent); still expose explicit
  `flush()`/`close()`.
- Node: a `installShutdownHooks()` helper (or `autoShutdown` init flag) wiring
  `beforeExit`/`SIGTERM`/`SIGINT` to `close()`; explicit `close()` still primary.
- C#: `Close()`/`Dispose()` already cover it; document the `using`/host-lifetime pattern.

## 5. Envelope reconciliation (all 5, no backend change)

Canonical shape = `envelope.rs`. Converge every SDK's emitted `ErrorItem`:

- **Browser (js):** currently omits `event_id`, `message`, `tags`, `user` on its
  `ErrorItem` type — add them (all default-tolerant server-side, so this is forward
  parity, not a fix).
- **Servers (node/python/csharp):** currently omit `breadcrumbs` (hardcoded `[]`) and
  `fingerprint` — add real breadcrumbs (§4.2) and an optional `fingerprint` override.
- **Flutter:** unify `beforeSend` from errors-only to **any-item** to match js
  (`sauron_options.dart` — minor behavioral change, called out in the changelog).
- **Golden-envelope fixture tests:** add to **Node** and **C#** (js/flutter/python already
  have them). Each asserts the SDK serializes the shared golden envelope byte-compatibly
  with `envelope.rs::GOLDEN`. This becomes the standing guard against future drift.

`session_id`/`screen` remain client-only fields (servers don't track sessions/screens);
they stay absent (and default) on server envelopes.

## 6. Wiki

Update existing pages and add new ones. All in-repo `wiki/` (GitHub-wiki page naming;
**do not push to any remote wiki**).

**Update (match the new APIs):** `Browser-SDK.md`, `Flutter-SDK.md`, `Node-SDK.md`,
`Python-SDK.md`, `CSharp-SDK.md`, `Home.md`, `_Sidebar.md`, `Getting-Started.md`,
`Ingest-Wire-Contract.md` (add breadcrumbs/tags/user/fingerprint on error items,
transactions from servers, gzip).

**New pages:**

- `Framework-Integrations.md` — copy-paste middleware/recipes that set per-request scope
  and capture errors:
  - Node: Express, Fastify, Koa (request-scoped `withScope` + error handler + HTTP-timing
    `trackTransaction`).
  - Python: Flask, FastAPI/Starlette, Django (middleware + `with sauron.scope()`).
  - C#: ASP.NET Core middleware (`PushScope` per request, exception filter).
  - Browser: React error boundary, Vue `errorHandler`, Svelte — and a note on source maps.
- `Best-Practices.md` — event-naming conventions, PII scrubbing via `beforeSend`,
  sampling, tags vs context, `distinct_id` strategy (anonymous → identify alias),
  flush/shutdown discipline for short-lived processes.
- `Troubleshooting.md` — nothing showing up (DSN/flush/no-op-before-init), disabled no-op
  mode, gzip verification, retry/queue behavior, concurrency/scope-leak pitfalls.
- `Capabilities.md` — the feature-parity matrix as the single source of truth
  (init/track/capture/identify/scope/breadcrumbs/transactions/auto-capture/queue/retry/
  gzip/before-send per SDK).

`_Sidebar.md`, `Home.md`, and the top-level `README.md` link the new pages.

## 7. Versioning

Synchronize all five SDKs to **v0.3.0** as a coordinated "parity release":

- `sdks/js/package.json`, `sdks/node/package.json` → `0.3.0`
- `sdks/python/pyproject.toml` + hardcoded `SDK_VERSION` in `_client.py` → `0.3.0`
- `sdks/csharp/Sauron/Sauron.csproj` `<Version>` → `0.3.0`
- `sdks/flutter/pubspec.yaml` → `0.3.0` (+ `CHANGELOG.md` entry)

Each SDK's `README.md` and `CHANGELOG` (where present) note the new capabilities.

## 8. Testing & verification

**Per SDK unit tests (new/extended):**

- Scope isolation under concurrency — two interleaved `withScope`/`scope()` blocks do not
  leak user/tags/breadcrumbs into each other.
- Breadcrumb ring — bound respected, oldest dropped, attached to captured errors.
- `beforeSend` drop (`-> null`) and mutate; `beforeBreadcrumb`.
- Gzip round-trip — body above threshold is gzipped with the right header and
  decompresses to the original JSON.
- Retry policy — 429 honors `Retry-After`; 5xx backs off; 400 drops (Node especially).
- Golden-envelope fixture — byte-parity with the shared golden.
- Auto-capture (opt-in) — handler installed only when enabled; chains/preserves prior
  behavior.

**Build/test gates:** js `vitest`, node `vitest` + `tsc`, python `pytest`, csharp
`dotnet build && dotnet test`, flutter `flutter test`.

**Live e2e (docker-compose):** extend each server example
(`examples/{node,python,csharp}-server`) to exercise scope + breadcrumbs +
`trackTransaction`, dispatch against the running ingest, and confirm in the dashboard:
breadcrumbs on the grouped issue, server transactions under Performance, tags/user on the
error, event under Events. Reuse the existing compose stack (API 10000 / ingest 10001 /
dashboard 10002).

**Wiki checks:** internal links resolve; every code snippet matches the shipped API.

**Process rules:** no git commits (leave in working tree — user's main-only /
stop-auto-commit preference); no pushing the wiki anywhere.

## 9. Non-goals (YAGNI)

- No backend / DB / migration / pipeline / ingest changes (already supports everything).
- No auto HTTP instrumentation inside the SDK core (framework recipes instead).
- No session replay, no new app types, no SSO/billing.
- No breaking changes to the existing client (browser/Flutter) public surface beyond the
  additive reconciliation fields and the Flutter `beforeSend` widening.

## 10. Defaults chosen (flip on request)

- Scope isolation = language async-local (not thread-local, not global-only).
- Disk-persistent queue = **opt-in, off** (in-memory bounded by default).
- Auto uncaught-error capture = **opt-in, off**.

## 11. Workstream decomposition (feeds the implementation plan)

Highly parallel; suggested streams (each independently testable):

1. **Node SDK** — scope, breadcrumbs, before-send, auto-capture, transactions, gzip,
   retry, queue, shutdown, golden test, v0.3.0.
2. **Python SDK** — same surface, `contextvars` + context managers + `atexit`.
3. **C# SDK** — same surface, `AsyncLocal` + `IDisposable` scope, golden test.
4. **Browser + Flutter reconciliation** — envelope fields, Flutter `beforeSend` widening,
   v0.3.0, CHANGELOG.
5. **Examples** — extend the three server examples (and confirm browser/Flutter still
   build) to demo the new surface.
6. **Wiki** — update 5 SDK pages + Home/Sidebar/Getting-Started/Ingest-Wire-Contract; add
   Framework-Integrations, Best-Practices, Troubleshooting, Capabilities; README links.
7. **Live e2e** — docker-compose verification of the whole thing.

Streams 1–3 are independent; 4 is independent; 5 depends on 1–4; 6 can proceed in
parallel and finalizes against the shipped APIs; 7 runs last.
