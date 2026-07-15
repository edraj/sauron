# Full SDK Parity + Wiki — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the three server SDKs (Node, Python, C#) up to the Browser/Flutter feature bar, reconcile the error-envelope shape across all five, and grow the wiki to match — SDK code + wiki only.

**Architecture:** One shared capability model (scope, breadcrumbs, before-send, opt-in auto-capture, transactions, gzip, retry, bounded/optional-disk queue) implemented in each language's native idiom (Node `AsyncLocalStorage`, Python `contextvars`, C# `AsyncLocal`). Every SDK converges on the identical wire shape defined in `backend/crates/sauron-core/src/envelope.rs`, guarded by a golden-envelope fixture test per SDK. No backend/DB/pipeline changes.

**Tech Stack:** TypeScript (tsup/vitest), Python 3 stdlib (pytest), C# net8.0 (`System.Text.Json`, xUnit/`dotnet test`), Dart/Flutter, Markdown wiki.

**Spec:** `docs/superpowers/specs/2026-07-15-sdk-full-parity-and-wiki-design.md`.

## Global Constraints

- **NO git commits.** Leave every change in the working tree (user's main-only / stop-auto-commit rule). Where a task's final step says "Gate," run the listed test/build command and confirm green — do **not** `git add`/`git commit`.
- **NO pushing the wiki** to any remote/GitHub wiki.
- **NO backend / DB / migration / pipeline / ingest changes.** The wire target is `backend/crates/sauron-core/src/envelope.rs` (already a tolerant superset). Read it; do not edit it.
- **Wire parity is law.** Every SDK must serialize the shared golden envelope byte-compatibly. Field names are snake_case on the wire (`distinct_id`, `event_id`, `http_status`, `duration_ms`, `raw_stacktrace`, `session_id`). Item `type` tags: `error | event | identify | breadcrumb_batch | transaction`.
- **Version floor:** all five SDKs ship as **v0.3.0** this release.
- **Breadcrumb ring default:** 100 (align servers to Flutter's 100; js stays 50 unless a task says otherwise).
- **Defaults:** scope isolation = async-local; disk-persistent queue = opt-in/off; auto uncaught-capture = opt-in/off.
- **Server SDKs stay dependency-light:** Python = stdlib only (no new runtime deps). Node = no new runtime deps (Node ≥18 `zlib`/`fetch`). C# = `System.*` only.
- **Known gotcha:** the Semgrep Guardian PreToolUse hook can block `Write`/`Edit` until logged into Semgrep. If blocked, apply file edits via `Bash` (heredoc/python) with the user's OK, as done in prior sessions. `Bash` itself is not blocked.
- **No-op discipline:** every dispatch API is a no-op before `init` / when the DSN is missing/disabled. New APIs must preserve this.

---

## File Structure

**Node (`sdks/node/`)** — new files: `src/scope.ts` (Scope + AsyncLocalStorage hub), `src/breadcrumbs.ts` (ring buffer type is small — fold into scope), `src/gzip.ts` (gzip helper), `src/queue.ts` (bounded in-memory + optional disk), `src/autocapture.ts` (opt-in handlers). Modified: `src/client.ts` (wire scope/breadcrumbs/transactions/before-send into capture/track), `src/transport.ts` (gzip + retry), `src/types.ts` (Transaction item, new options, breadcrumbs), `src/index.ts` (new exports), `package.json` (0.3.0). New tests under `test/`.

**Python (`sdks/python/sauron/`)** — new: `_scope.py` (Scope + contextvars), `_gzip.py`, `_queue.py`, `_autocapture.py`. Modified: `_client.py`, `_transport.py`, `__init__.py` (exports + `atexit`), `pyproject.toml` (0.3.0). New tests under `tests/`.

**C# (`sdks/csharp/Sauron/`)** — new: `Scope.cs` (Scope + `AsyncLocal`), `Breadcrumb.cs`, `Gzip.cs`, `Queue.cs`, `AutoCapture.cs`, `TransactionItem.cs`. Modified: `SauronClient.cs`, `SauronSdk.cs`, `Transport.cs`, `Envelope.cs`, `Sauron.csproj` (0.3.0). New tests under `Sauron.Tests/`.

**Browser (`sdks/js/`)** — modified: `src/types.ts` (add `event_id/message/tags/user` to `ErrorItem`), `src/client.ts` (emit them), `package.json` (0.3.0). Test: `test/envelope.test.ts`.

**Flutter (`sdks/flutter/`)** — modified: `lib/src/sauron_options.dart` + `lib/src/client.dart` (`beforeSend` widened to any item), `pubspec.yaml` (0.3.0), `CHANGELOG.md`. Test: `test/envelope_test.dart`.

**Examples (`examples/`)** — modified: `node-server/`, `python-server/`, `csharp-server/` (demo scope + breadcrumbs + `trackTransaction`).

**Wiki (`wiki/`)** — modified: 5 SDK pages + `Home.md`, `_Sidebar.md`, `Getting-Started.md`, `Ingest-Wire-Contract.md`. New: `Framework-Integrations.md`, `Best-Practices.md`, `Troubleshooting.md`, `Capabilities.md`. Modified: top-level `README.md`.

**Shared golden fixture:** the canonical JSON both the Rust `envelope.rs::GOLDEN` and each SDK test assert against. Reuse the existing shape from `envelope.rs:325` and the js/flutter/python tests. Extend it to include a server-shaped error item (breadcrumbs + tags + user + fingerprint) and a transaction item so every SDK exercises the reconciled shape.

---

# Workstream A — Node SDK (`@sauron/node`)

Independent. Reference implementation for B and C. Reads: `sdks/node/src/{client,transport,types,index}.ts`, `sdks/js/src/{scope,client}.ts` (idiom reference), `backend/crates/sauron-core/src/envelope.rs`.

### Task A1: Scope + async-local hub

**Files:**
- Create: `sdks/node/src/scope.ts`
- Modify: `sdks/node/src/types.ts` (add `ScopeData`, `Breadcrumb`, `User` if absent)
- Test: `sdks/node/test/scope.test.ts`

**Interfaces:**
- Produces: `class Scope { setUser(u: User|null); setTag(k,v); setTags(o); setContext(key,obj); setExtra(k,v); addBreadcrumb(b: BreadcrumbInput); clone(): Scope; applyToErrorItem(item); data: ScopeData }`. Module fns: `getCurrentScope(): Scope`, `getGlobalScope(): Scope`, `withScope<T>(cb: (s: Scope) => T): T`, `configureScope(cb: (s: Scope) => void): void`, `runWithAsyncScope(cb)`. Backed by `const als = new AsyncLocalStorage<Scope>()`.
- `ScopeData = { user?: User|null; tags: Record<string,string>; contexts: Record<string,unknown>; extra: Record<string,unknown>; breadcrumbs: Breadcrumb[] }`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test** (`test/scope.test.ts`)

```ts
import { describe, it, expect } from 'vitest';
import { getGlobalScope, withScope, getCurrentScope, Scope } from '../src/scope';

describe('scope', () => {
  it('merges global tags under a child scope', () => {
    getGlobalScope().setTag('env', 'prod');
    withScope((s) => {
      s.setTag('req', '42');
      const item: any = { type: 'error', tags: {} };
      getCurrentScope().applyToErrorItem(item);
      expect(item.tags).toEqual({ env: 'prod', req: '42' });
    });
  });

  it('isolates concurrent scopes (no leak)', async () => {
    const seen: string[] = [];
    await Promise.all([
      withScope(async (s) => { s.setTag('id', 'A'); await tick(); seen.push(getCurrentScope().data.tags.id); }),
      withScope(async (s) => { s.setTag('id', 'B'); await tick(); seen.push(getCurrentScope().data.tags.id); }),
    ]);
    expect(seen.sort()).toEqual(['A', 'B']);
  });

  it('bounds breadcrumbs at maxBreadcrumbs', () => {
    const s = new Scope(3);
    for (let i = 0; i < 5; i++) s.addBreadcrumb({ message: String(i) });
    expect(s.data.breadcrumbs.map((b) => b.message)).toEqual(['2', '3', '4']);
  });
});
const tick = () => new Promise((r) => setTimeout(r, 5));
```

- [ ] **Step 2: Run to verify it fails** — `cd sdks/node && npx vitest run test/scope.test.ts` → FAIL (module not found).

- [ ] **Step 3: Implement `src/scope.ts`.** `withScope` clones the current scope, runs `als.run(child, cb)`, and returns the result (supports async cb — return the promise so `als` context spans awaits). `applyToErrorItem` merges `{...global.tags, ...current.tags}`, sets `user` (current ?? global), and fills `breadcrumbs` (global first, then current, capped). Global scope is a module singleton; `getCurrentScope()` returns `als.getStore() ?? globalScope`. Ring buffer: `push` then `if (len > max) shift()`.

- [ ] **Step 4: Run to verify pass** — `npx vitest run test/scope.test.ts` → PASS.

- [ ] **Step 5: Gate** — `npx vitest run test/scope.test.ts && npx tsc --noEmit -p tsconfig.json`.

### Task A2: Breadcrumbs + before-breadcrumb, wired into client

**Files:**
- Modify: `sdks/node/src/client.ts` (add `addBreadcrumb`), `sdks/node/src/index.ts` (export `addBreadcrumb`, `setUser`, `setTag`, `setContext`, `withScope`, `configureScope`), `sdks/node/src/types.ts` (`InitOptions.maxBreadcrumbs`, `InitOptions.beforeBreadcrumb`)
- Test: `sdks/node/test/breadcrumbs.test.ts`

**Interfaces:**
- Produces: top-level `addBreadcrumb(b: BreadcrumbInput): void` (adds to current scope after `beforeBreadcrumb`), and re-exported scope fns from A1. `BreadcrumbInput = { type?: string; category?: string; message?: string; level?: Level; data?: Record<string, unknown> }`; stored crumb stamps `timestamp` (ISO).
- Consumes: A1 scope.

- [ ] **Step 1: Failing test** — assert (a) `addBreadcrumb({message:'x'})` then a captured error carries that crumb in `item.breadcrumbs[0].message === 'x'`; (b) a `beforeBreadcrumb` returning `null` drops the crumb; (c) crumbs carry an ISO `timestamp`. Use the existing `fetchImpl`/sender test seam (see `test/transport.test.ts`) to capture the emitted envelope JSON.

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement.** `addBreadcrumb` runs `beforeBreadcrumb(crumb)` (if configured), drops on `null`, else `getCurrentScope().addBreadcrumb(crumb)`. In `captureException`/`captureMessage`, after building the error item, call `getCurrentScope().applyToErrorItem(item)` so breadcrumbs/tags/user attach (replaces the hardcoded `breadcrumbs: []` at `client.ts:135`).

- [ ] **Step 4: Run → PASS.**

- [ ] **Step 5: Gate** — `npx vitest run test/breadcrumbs.test.ts && npx tsc --noEmit`.

### Task A3: before-send hook (any item)

**Files:**
- Modify: `sdks/node/src/client.ts`, `sdks/node/src/types.ts` (`InitOptions.beforeSend?: (item: EnvelopeItem, hint?: unknown) => EnvelopeItem | null`)
- Test: `sdks/node/test/beforesend.test.ts`

**Interfaces:**
- Produces: `beforeSend` runs on **every** item (`error|event|identify|transaction`) just before it is enqueued for transport; returning `null` drops it; a returned object replaces it.
- Consumes: nothing new.

- [ ] **Step 1: Failing test** — a `beforeSend` that redacts `properties.email` mutates a tracked event; a `beforeSend` returning `null` for `type==='error'` yields an envelope with no error item. Capture via sender seam.

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement.** Add a single `applyBeforeSend(item)` chokepoint in the client's enqueue path; skip enqueue when it returns `null`.

- [ ] **Step 4: Run → PASS.**

- [ ] **Step 5: Gate** — `npx vitest run test/beforesend.test.ts`.

### Task A4: Transactions (`trackTransaction`)

**Files:**
- Modify: `sdks/node/src/types.ts` (add `TransactionItem` to `EnvelopeItem` union + `TransactionInput`), `sdks/node/src/client.ts`, `sdks/node/src/index.ts`
- Test: `sdks/node/test/transaction.test.ts`

**Interfaces:**
- Produces: `trackTransaction(input: TransactionInput): void`. `TransactionInput = { name: string; op?: string; duration_ms: number; status?: string; http_method?: string; http_status?: number; url?: string; distinct_id?: string }`. Emits `{ type:'transaction', name, op: op??'custom', duration_ms, status?, http_method?, http_status?, url?, distinct_id? (?? scoped user id), timestamp }` — matching `envelope.rs::TransactionItem`.
- Consumes: A1 scope (for `distinct_id` fallback), A3 before-send.

- [ ] **Step 1: Failing test** — `trackTransaction({name:'GET /u', op:'http', duration_ms:12.5, http_status:200})` emits a `transaction` item with those fields and `op==='http'`; omitting `op` defaults to `'custom'`.

- [ ] **Step 2: Run → FAIL.** → **Step 3: Implement.** → **Step 4: PASS.**

- [ ] **Step 5: Gate** — `npx vitest run test/transaction.test.ts`.

### Task A5: Gzip transport

**Files:**
- Create: `sdks/node/src/gzip.ts`
- Modify: `sdks/node/src/transport.ts` (gzip body over threshold; set `Content-Encoding: gzip`), `sdks/node/src/types.ts` (`InitOptions.gzipThresholdBytes` default 1024)
- Test: `sdks/node/test/gzip.test.ts`

**Interfaces:**
- Produces: `maybeGzip(body: string, threshold: number): { body: Buffer|string; headers: Record<string,string> }` using `zlib.gzipSync`. Sub-threshold → passthrough (no header).

- [ ] **Step 1: Failing test** — body > threshold → returns a `Buffer` and `{'content-encoding':'gzip'}`, and `zlib.gunzipSync(result.body).toString()` equals the original; body < threshold → passthrough string, no header.

- [ ] **Step 2–4: TDD** (`zlib.gzipSync`).

- [ ] **Step 5: Gate** — `npx vitest run test/gzip.test.ts`.

### Task A6: Retry / backoff (Node parity)

**Files:**
- Modify: `sdks/node/src/transport.ts`
- Test: `sdks/node/test/retry.test.ts`

**Interfaces:**
- Produces: transport retries transient failures with exponential backoff + jitter (cap 30s): retry on 408/413/429/5xx and network errors, honor `Retry-After` (seconds or HTTP-date) on 429, **drop** on 400/401/403/404, give up after `maxRetries` (default 3) and drop the batch. Deterministic in tests via an injected `sleep`/clock seam and the existing `fetchImpl` seam.

- [ ] **Step 1: Failing test** — a `fetchImpl` that returns 429 with `Retry-After: 0` once then 200 results in exactly 2 calls and the batch delivered; a `fetchImpl` returning 400 results in 1 call and a dropped batch (no retry). Inject a no-op sleep.

- [ ] **Step 2–4: TDD.** Replace the "drop on any transient failure" logic (`transport.ts:128`) with the policy table above.

- [ ] **Step 5: Gate** — `npx vitest run test/retry.test.ts`.

### Task A7: Bounded queue + opt-in disk persistence

**Files:**
- Create: `sdks/node/src/queue.ts`
- Modify: `sdks/node/src/transport.ts` (buffer through the queue), `sdks/node/src/types.ts` (`InitOptions.maxQueueBytes` default 1_048_576, `InitOptions.offlineDir?`)
- Test: `sdks/node/test/queue.test.ts`

**Interfaces:**
- Produces: `class BoundedQueue { push(item): void; drain(): Item[]; get bytes(): number }` — drop-oldest when over `maxQueueBytes`. When `offlineDir` is set, persist pending envelopes FIFO (`fs`), delete on successful send, and reload on construction (at-least-once across restarts). Default (no dir) = memory only.

- [ ] **Step 1: Failing test** — pushing past `maxQueueBytes` drops oldest and keeps `bytes <= max`; with a temp `offlineDir`, a persisted item written by one instance is drained by a fresh instance; delivered items are removed from disk.

- [ ] **Step 2–4: TDD** (temp dir via `os.tmpdir()` + `fs.mkdtempSync`).

- [ ] **Step 5: Gate** — `npx vitest run test/queue.test.ts`.

### Task A8: Opt-in auto-capture + shutdown hooks

**Files:**
- Create: `sdks/node/src/autocapture.ts`
- Modify: `sdks/node/src/client.ts` (install when `autoCaptureUnhandled`), `sdks/node/src/types.ts` (`InitOptions.autoCaptureUnhandled?` default false, `InitOptions.autoShutdown?` default false), `sdks/node/src/index.ts` (export `installShutdownHooks`)
- Test: `sdks/node/test/autocapture.test.ts`

**Interfaces:**
- Produces: `installAutoCapture(client)` registers `process.on('uncaughtException'|'unhandledRejection')` (capture with `mechanism.handled=false`, then re-emit default behavior); returns an uninstaller. `installShutdownHooks(client)` wires `beforeExit`/`SIGTERM`/`SIGINT` to `close()`. Both are idempotent and only active when opted in.

- [ ] **Step 1: Failing test** — with `autoCaptureUnhandled:true`, emitting a fake `uncaughtException` (call the registered listener directly) enqueues an error item with `mechanism.handled===false`; with it false, no listener is registered. Assert via a spy on `process.on`/the sender seam. `close()` is called by the shutdown listener.

- [ ] **Step 2–4: TDD.** Do not let the handler swallow the process — after flushing, rethrow / preserve exit semantics (guard against recursive capture).

- [ ] **Step 5: Gate** — `npx vitest run test/autocapture.test.ts`.

### Task A9: Node reconciliation + golden test + v0.3.0

**Files:**
- Modify: `sdks/node/src/client.ts` (error item includes `fingerprint?`, real `breadcrumbs`, `tags`, `user` from scope), `sdks/node/src/index.ts` (final export surface), `sdks/node/package.json` (`0.3.0`), `sdks/node/README.md`
- Test: `sdks/node/test/envelope.test.ts` (golden)

**Interfaces:**
- Consumes: A1–A8. Produces the final public surface: `init, getClient, track, captureException, captureMessage, identify, trackTransaction, addBreadcrumb, setUser, setTag, setTags, setContext, setExtra, configureScope, withScope, installShutdownHooks, flush, close, SauronClient, describeError, parseDsn, DsnError`.

- [ ] **Step 1: Golden test** — build the shared golden envelope (an error item with exception + breadcrumbs + tags + user + fingerprint, an event, an identify, a transaction) and assert the serialized JSON deep-equals the shared fixture (snake_case keys, item `type` tags correct, no `undefined` leaking as `null` where the fixture omits).

- [ ] **Step 2: Run → FAIL** on any drift.

- [ ] **Step 3: Reconcile** the client's error-item builder to the canonical `envelope.rs::ErrorItem` field set; bump versions.

- [ ] **Step 4: Full gate** — `cd sdks/node && npx vitest run && npx tsc --noEmit`. Expected: all green.

- [ ] **Step 5: Gate** (no commit).

---

# Workstream B — Python SDK (`sauron`)

Independent. Mirrors A in behavior; idiom = `contextvars` + context managers + `atexit`. Stdlib only. Reads: `sdks/python/sauron/{_client,_transport,__init__}.py`, `sdks/python/tests/*`.

### Task B1: Scope + contextvars

**Files:**
- Create: `sdks/python/sauron/_scope.py`
- Test: `sdks/python/tests/test_scope.py`

**Interfaces:**
- Produces: `class Scope` with `set_user`, `set_tag`, `set_tags`, `set_context`, `set_extra`, `add_breadcrumb`, `clone()`, `apply_to_error(item: dict)`. Module: `get_global_scope()`, `get_current_scope()`, `push_scope()`/`pop_scope()`, and `@contextmanager def scope()` (push on enter, pop on exit). Backed by `_current: ContextVar[Optional[Scope]] = ContextVar('sauron_scope', default=None)`. `apply_to_error` merges `{**global.tags, **current.tags}`, sets `user`, caps breadcrumbs at `max_breadcrumbs` (100).

- [ ] **Step 1: Failing test** (`tests/test_scope.py`) — global tag + `with scope()` child tag both land on an error dict via `apply_to_error`; breadcrumb ring caps at N (oldest dropped); two `contextvars.copy_context()`-run scopes don't leak tags into each other.

- [ ] **Step 2: `python -m pytest tests/test_scope.py -q` → FAIL.**

- [ ] **Step 3: Implement `_scope.py`.**

- [ ] **Step 4: → PASS.**

- [ ] **Step 5: Gate** — `python -m pytest tests/test_scope.py -q`.

### Task B2: Breadcrumbs + before-breadcrumb

**Files:** Modify `sdks/python/sauron/_client.py`, `sdks/python/sauron/__init__.py` (export `add_breadcrumb`, `set_user`, `set_tag`, `set_context`, `set_extra`, `scope`, `push_scope`, `configure_scope`), add `before_breadcrumb` to `init`. Test: `tests/test_breadcrumbs.py`.

**Interfaces:** Produces `add_breadcrumb(*, type=None, category=None, message=None, level=None, data=None)`; runs `before_breadcrumb` (drop on `None`); crumbs stamp ISO `timestamp`. `capture_exception`/`capture_message` call `apply_to_error` (fills the `[]` at `_client.py:158`).

- [ ] **Step 1: Failing test** — breadcrumb attaches to a captured error via the injected `sender`; `before_breadcrumb` returning `None` drops it. → **2: FAIL → 3: impl → 4: PASS.**
- [ ] **Step 5: Gate** — `python -m pytest tests/test_breadcrumbs.py -q`.

### Task B3: before-send (any item)

**Files:** Modify `_client.py`, `__init__.py` (`init(before_send=...)`). Test: `tests/test_beforesend.py`.

**Interfaces:** Produces `before_send(item: dict, hint=None) -> dict | None` on every item; `None` drops. Single chokepoint before enqueue.

- [ ] **Steps 1–4: TDD** — redact a property; drop an error. → **Step 5: Gate** `python -m pytest tests/test_beforesend.py -q`.

### Task B4: Transactions

**Files:** Modify `_client.py`, `__init__.py`. Test: `tests/test_transaction.py`.

**Interfaces:** Produces `track_transaction(name, *, op='custom', duration_ms, status=None, http_method=None, http_status=None, url=None, distinct_id=None)` → `transaction` item (snake_case), `distinct_id` falls back to scoped user id.

- [ ] **Steps 1–4: TDD.** → **Step 5: Gate** `python -m pytest tests/test_transaction.py -q`.

### Task B5: Gzip

**Files:** Create `sdks/python/sauron/_gzip.py`; modify `_transport.py` (gzip over threshold + `Content-Encoding: gzip`), `__init__.py` (`gzip_threshold_bytes=1024`). Test: `tests/test_gzip.py`.

**Interfaces:** Produces `maybe_gzip(body: bytes, threshold: int) -> tuple[bytes, dict]` via `gzip.compress`; sub-threshold passthrough. The `sender` seam receives the (possibly gzipped) body + headers.

- [ ] **Steps 1–4: TDD** — `gzip.decompress(out) == body` above threshold; passthrough below. → **Step 5: Gate** `python -m pytest tests/test_gzip.py -q`.

### Task B6: Retry policy alignment

**Files:** Modify `_transport.py`. Test: `tests/test_retry.py`.

**Interfaces:** Align the existing backoff (`_transport.py:158`) to the shared table: retry 408/413/429/5xx + network, honor `Retry-After` on 429, drop on 400/401/403/404, `max_retries` default 3, cap 30s. Deterministic via injected sleep + `sender`.

- [ ] **Steps 1–4: TDD** — 429 then 200 = 2 sends; 400 = 1 send, dropped. → **Step 5: Gate** `python -m pytest tests/test_retry.py -q`.

### Task B7: Bounded queue + opt-in disk

**Files:** Create `sdks/python/sauron/_queue.py`; modify `_transport.py`, `__init__.py` (`max_queue_bytes=1_048_576`, `offline_path=None`). Test: `tests/test_queue.py`.

**Interfaces:** Produces `class BoundedQueue` (drop-oldest by bytes); with `offline_path`, persist FIFO (one file per envelope under the dir), reload on init, delete on send. Default memory-only.

- [ ] **Steps 1–4: TDD** — over-cap drops oldest; `tmp_path` fixture round-trips a persisted item to a fresh instance. → **Step 5: Gate** `python -m pytest tests/test_queue.py -q`.

### Task B8: Opt-in auto-capture + atexit

**Files:** Create `sdks/python/sauron/_autocapture.py`; modify `_client.py`/`__init__.py` (`auto_capture_unhandled=False`), register `atexit` flush in `init`. Test: `tests/test_autocapture.py`.

**Interfaces:** Produces `install_excepthook(client)` chaining the previous `sys.excepthook` (+ optional `threading.excepthook`), capturing with `mechanism.handled=False`; returns an uninstaller. `init` registers `atexit.register(close)` (idempotent).

- [ ] **Steps 1–4: TDD** — call the installed hook with a fake `(exc_type, exc, tb)`; assert an error item with `handled is False` reaches the sender; prior hook still invoked. → **Step 5: Gate** `python -m pytest tests/test_autocapture.py -q`.

### Task B9: Python reconciliation + golden + 0.3.0

**Files:** Modify `_client.py` (error item = canonical field set incl. `fingerprint`, real breadcrumbs/tags/user), `pyproject.toml` (`0.3.0`), `_client.py:18` `SDK_VERSION="0.3.0"`, `README.md`. Test: extend `tests/test_envelope.py` with the shared golden (server error item + transaction).

- [ ] **Step 1: Golden test asserting** the serialized envelope equals the shared fixture. → **2: FAIL → 3: reconcile + bump → 4: PASS.**
- [ ] **Step 5: Full gate** — `cd sdks/python && python -m pytest -q`. Expected: all green.

---

# Workstream C — C# SDK (`Sauron`)

Independent. Idiom = `AsyncLocal<Scope>` + `IDisposable` scope handle. `System.*` only. Reads: `sdks/csharp/Sauron/{SauronClient,SauronSdk,Transport,Envelope}.cs`, `Sauron.Tests/*`.

### Task C1: Scope + AsyncLocal

**Files:** Create `sdks/csharp/Sauron/Scope.cs`, `sdks/csharp/Sauron/Breadcrumb.cs`. Test: `sdks/csharp/Sauron.Tests/ScopeTests.cs`.

**Interfaces:**
- Produces: `sealed class Scope { SauronUser? User; Dictionary<string,string> Tags; Dictionary<string,object?> Contexts; Dictionary<string,object?> Extra; List<Breadcrumb> Breadcrumbs; Scope Clone(); void ApplyToError(ErrorItem item); void AddBreadcrumb(Breadcrumb b, int max); }`. A `ScopeManager` with `private static readonly AsyncLocal<Scope?> _current`, `Global` singleton, `Current => _current.Value ?? Global`, `IDisposable PushScope()` (sets `_current.Value = Current.Clone()`, restores on `Dispose`). `Breadcrumb { Type, Category, Message, Level, Timestamp (DateTimeOffset), Data }`.

- [ ] **Step 1: Failing test** (`ScopeTests.cs`, xUnit) — global tag + a `using PushScope()` child tag both land via `ApplyToError`; breadcrumb ring caps at N; a nested `using` restores the parent scope on dispose.

- [ ] **Step 2: `cd sdks/csharp && dotnet test --filter ScopeTests` → FAIL (build).**

- [ ] **Step 3: Implement `Scope.cs` + `Breadcrumb.cs`.**

- [ ] **Step 4: → PASS.** → **Step 5: Gate** `dotnet test --filter ScopeTests`.

### Task C2: Breadcrumbs + facade scope API

**Files:** Modify `SauronClient.cs` (AddBreadcrumb, apply scope on capture), `SauronSdk.cs` (static `AddBreadcrumb`, `SetUser`, `SetTag`, `SetTags`, `SetContext`, `SetExtra`, `PushScope`, `BeforeBreadcrumb` option). Test: `Sauron.Tests/BreadcrumbTests.cs`.

**Interfaces:** Produces `SauronSdk.AddBreadcrumb(Breadcrumb)`, scope setters, `IDisposable SauronSdk.PushScope()`. `CaptureException`/`CaptureMessage` call `Scope.ApplyToError` (adds the breadcrumbs field absent today). Option `BeforeBreadcrumb: Func<Breadcrumb, Breadcrumb?>?`.

- [ ] **Steps 1–4: TDD** — breadcrumb attaches to a captured error via the `HttpMessageHandler` seam (`CapturingHandler`); `BeforeBreadcrumb` returning `null` drops it. → **Step 5: Gate** `dotnet test --filter BreadcrumbTests`.

### Task C3: before-send (any item)

**Files:** Modify `SauronClient.cs`, `SauronOptions` (add `BeforeSend: Func<object, object?>?` over the item DTO). Test: `Sauron.Tests/BeforeSendTests.cs`.

**Interfaces:** Produces a `BeforeSend` chokepoint on every item; `null` drops. (Item DTOs are the `Envelope.cs` records.)

- [ ] **Steps 1–4: TDD** — redact a property; drop an error. → **Step 5: Gate** `dotnet test --filter BeforeSendTests`.

### Task C4: Transactions

**Files:** Create `sdks/csharp/Sauron/TransactionItem.cs` (record); modify `Envelope.cs` (item union/serialization), `SauronClient.cs`/`SauronSdk.cs` (`TrackTransaction`). Test: `Sauron.Tests/TransactionTests.cs`.

**Interfaces:** Produces `SauronSdk.TrackTransaction(string name, double durationMs, string op = "custom", string? status = null, string? httpMethod = null, int? httpStatus = null, string? url = null, string? distinctId = null)` → `transaction` item with snake_case `JsonPropertyName` (`duration_ms`, `http_method`, `http_status`, `distinct_id`).

- [ ] **Steps 1–4: TDD** — emitted JSON has `"type":"transaction"`, `"duration_ms":12.5`, `"op":"http"`. → **Step 5: Gate** `dotnet test --filter TransactionTests`.

### Task C5: Gzip

**Files:** Create `sdks/csharp/Sauron/Gzip.cs`; modify `Transport.cs` (gzip over threshold, `Content-Encoding: gzip`), `SauronOptions` (`GzipThresholdBytes = 1024`). Test: `Sauron.Tests/GzipTests.cs`.

**Interfaces:** Produces `static byte[] MaybeGzip(byte[] body, int threshold, out bool gzipped)` via `GZipStream`; sub-threshold passthrough.

- [ ] **Steps 1–4: TDD** — round-trip decompress above threshold; passthrough below; header set only when gzipped. → **Step 5: Gate** `dotnet test --filter GzipTests`.

### Task C6: Retry policy alignment

**Files:** Modify `Transport.cs`. Test: `Sauron.Tests/RetryPolicyTests.cs`.

**Interfaces:** Align existing retry (`Transport.cs:110`) to the shared table (408/413/429/5xx + network, `Retry-After` on 429, drop on 4xx-non-retryable, max 3, cap 30s). Deterministic via a queued-response `HttpMessageHandler` + injected delay.

- [ ] **Steps 1–4: TDD** — 429→200 = 2 sends; 400 = 1 send dropped. → **Step 5: Gate** `dotnet test --filter RetryPolicyTests`.

### Task C7: Bounded queue + opt-in disk

**Files:** Create `sdks/csharp/Sauron/Queue.cs`; modify `Transport.cs`, `SauronOptions` (`MaxQueueBytes = 1_048_576`, `OfflineDir = null`). Test: `Sauron.Tests/QueueTests.cs`.

**Interfaces:** Produces `BoundedQueue` (drop-oldest by bytes); `OfflineDir` set ⇒ persist FIFO to disk, reload on init, delete on send. Default memory-only.

- [ ] **Steps 1–4: TDD** — over-cap drop; temp-dir round-trip to a fresh instance. → **Step 5: Gate** `dotnet test --filter QueueTests`.

### Task C8: Opt-in auto-capture

**Files:** Create `sdks/csharp/Sauron/AutoCapture.cs`; modify `SauronClient.cs`/`SauronSdk.cs` (`AutoCaptureUnhandled = false`). Test: `Sauron.Tests/AutoCaptureTests.cs`.

**Interfaces:** Produces `InstallHandlers(client)` wiring `AppDomain.CurrentDomain.UnhandledException` + `TaskScheduler.UnobservedTaskException` (capture `handled=false`); returns an uninstaller (unsubscribe on `Dispose`).

- [ ] **Steps 1–4: TDD** — raise the event via a test-visible entry point; assert an error item with `handled==false`; only wired when opted in. → **Step 5: Gate** `dotnet test --filter AutoCaptureTests`.

### Task C9: C# reconciliation + golden + 0.3.0

**Files:** Modify `Envelope.cs`/`SauronClient.cs` (error item = canonical set incl. `fingerprint`, real breadcrumbs/tags/user), `Sauron.csproj` (`<Version>0.3.0</Version>`). Test: `Sauron.Tests/EnvelopeGoldenTests.cs`.

- [ ] **Step 1: Golden test** — serialize the shared golden envelope; assert JSON equals the fixture (snake_case, `type` tags). → **2: FAIL → 3: reconcile + bump → 4: PASS.**
- [ ] **Step 5: Full gate** — `cd sdks/csharp && dotnet build && dotnet test`. Expected: all green.

---

# Workstream D — Browser + Flutter reconciliation

Independent of A–C. Small, additive.

### Task D1: Browser error-item fields + golden + 0.3.0

**Files:** Modify `sdks/js/src/types.ts` (`ErrorItem` gains `event_id?: string; message?: string; tags?: Record<string,unknown>; user?: UserPayload`), `sdks/js/src/client.ts` (populate them from scope/hint), `sdks/js/package.json` (`0.3.0`). Test: extend `sdks/js/test/envelope.test.ts`.

**Interfaces:** Produces a browser `ErrorItem` that carries `event_id/message/tags/user` when present (all optional; omitted → absent, backend defaults them). No breaking change to existing exports.

- [ ] **Step 1: Extend the golden test** to assert an error item with `event_id`+`tags`+`user` serializes those keys; existing minimal-error assertions still pass.
- [ ] **Step 2: Run → FAIL.** → **3: add fields → 4: PASS.**
- [ ] **Step 5: Gate** — `cd sdks/js && npx vitest run`. Expected: 36+ tests green.

### Task D2: Flutter before-send widening + 0.3.0

**Files:** Modify `sdks/flutter/lib/src/sauron_options.dart` (`BeforeSendCallback` widened from `ErrorItem? Function(ErrorItem)` to any-item: `Object? Function(Object item)` returning the item or `null`), `sdks/flutter/lib/src/client.dart` (apply on every item, not just errors — `client.dart:162`), `sdks/flutter/pubspec.yaml` (`0.3.0`), `sdks/flutter/CHANGELOG.md`. Test: extend `sdks/flutter/test/envelope_test.dart` / a before-send test.

**Interfaces:** Produces an any-item `beforeSend`; **behavioral change** (was errors-only) — call it out in `CHANGELOG.md`. Existing error-only usage still works (an error is an item).

- [ ] **Step 1: Failing test** — a `beforeSend` returning `null` for an event drops it (previously events bypassed the hook).
- [ ] **Step 2: → FAIL → 3: widen → 4: PASS.**
- [ ] **Step 5: Gate** — `cd sdks/flutter && flutter test`. Expected: all green.

### Task D3: Shared golden fixture sync

**Files:** Modify `backend/crates/sauron-core/src/envelope.rs` **tests only** (`GOLDEN` const) is **read-only reference** — do NOT edit backend. Instead record the shared golden JSON as a doc block in each SDK test. Verify the Rust `deserializes_golden_envelope` still passes unchanged: `cd backend && cargo test -p sauron-core envelope`. Expected: green (no code change; confirms the reconciled SDK shape still deserializes).

- [ ] **Step 1: Run** `cd backend && cargo test -p sauron-core` → PASS (guard that nothing regressed the contract).

---

# Workstream E — Examples (depends on A–D)

### Task E1: Node example — scope + breadcrumbs + transaction

**Files:** Modify `examples/node-server/index.*`, `examples/node-server/README.md`.

- [ ] Demo `withScope`/`setUser`/`setTag`, `addBreadcrumb` before a captured error, and one `trackTransaction`. Keep it copy-pasteable, DSN from `SAURON_DSN`, `await close()` at exit.
- [ ] **Gate:** `cd examples/node-server && npm install && npm run typecheck` (and `npm start` against the compose ingest if available).

### Task E2: Python example

**Files:** Modify `examples/python-server/main.py`, `README.md`.
- [ ] Demo `with sauron.scope():`, `set_user`/`set_tag`, `add_breadcrumb`, `track_transaction`; rely on `atexit` or explicit `flush()/close()`.
- [ ] **Gate:** `cd examples/python-server && python -c "import ast,sys; ast.parse(open('main.py').read())"` + `python main.py` against compose ingest if available.

### Task E3: C# example

**Files:** Modify `examples/csharp-server/Program.cs`, `README.md`.
- [ ] Demo `using SauronSdk.PushScope()`, `SetUser`/`SetTag`, `AddBreadcrumb`, `TrackTransaction`; `SauronSdk.Close()` at exit.
- [ ] **Gate:** `cd examples/csharp-server && dotnet build` (+ `dotnet run` against compose ingest if available).

---

# Workstream F — Wiki (parallel; finalize against shipped APIs)

### Task F1: Update the 5 SDK pages

**Files:** Modify `wiki/{Browser-SDK,Flutter-SDK,Node-SDK,Python-SDK,CSharp-SDK}.md`.
- [ ] Add the new surface to each: scope (`setUser/setTag/setContext` + `withScope`/`scope()`/`PushScope`), `addBreadcrumb`, `beforeSend`/`beforeBreadcrumb`, `trackTransaction`, gzip/retry/queue options, opt-in `autoCaptureUnhandled`, shutdown. Every snippet must match the **shipped** signatures (read the sources after A–D land).
- [ ] **Gate:** links resolve; snippets compile against the shipped API (spot-check).

### Task F2: `Framework-Integrations.md`

**Files:** Create `wiki/Framework-Integrations.md`.
- [ ] Copy-paste recipes: Express/Fastify/Koa (Node), Flask/FastAPI/Django (Python), ASP.NET Core middleware (C#), React error boundary / Vue `errorHandler` / Svelte (Browser). Each sets per-request scope + captures errors + times requests with `trackTransaction`.

### Task F3: `Best-Practices.md`

**Files:** Create `wiki/Best-Practices.md`.
- [ ] Event naming, PII scrubbing via `beforeSend`, sampling, tags vs context, `distinct_id` (anonymous → identify alias), flush/shutdown for short-lived processes.

### Task F4: `Troubleshooting.md`

**Files:** Create `wiki/Troubleshooting.md`.
- [ ] Nothing showing up (DSN/flush/no-op-before-init), disabled no-op mode, gzip verification, retry/queue behavior, concurrency/scope-leak pitfalls, version check.

### Task F5: `Capabilities.md` + nav wiring

**Files:** Create `wiki/Capabilities.md`; modify `wiki/Home.md`, `wiki/_Sidebar.md`, `wiki/Getting-Started.md`, `wiki/Ingest-Wire-Contract.md`, top-level `README.md`.
- [ ] Publish the parity matrix (all ✅ after this release). Link the 4 new pages from Home/Sidebar/README. Update `Ingest-Wire-Contract.md` for breadcrumbs/tags/user/fingerprint on error items, transactions from servers, and gzip.
- [ ] **Gate:** every internal wiki link resolves to a real file.

---

# Workstream G — Live end-to-end (runs last)

### Task G1: docker-compose e2e

- [ ] Bring up the stack (`docker compose up --build`; API 10000 / ingest 10001 / dashboard 10002 per `.env`).
- [ ] Run each server example (E1–E3) against the ingest with a real app DSN.
- [ ] Confirm in the dashboard: the captured error appears under **Exceptions** **with breadcrumbs + tags + user**, the tracked event under **Events**, the server transaction under **Performance**, and the identified person under **Users**.
- [ ] Confirm gzip path works (dispatch a large batch; ingest accepts `Content-Encoding: gzip`).
- [ ] Record results in the session memory ([[sauron-project]]); leave everything uncommitted.

---

## Self-Review

**Spec coverage:** §4.1 scope → A1/B1/C1; §4.2 breadcrumbs → A2/B2/C2; §4.3 before-send → A3/B3/C3; §4.4 auto-capture → A8/B8/C8; §4.5 transactions → A4/B4/C4; §4.6 gzip → A5/B5/C5; §4.7 retry → A6/B6/C6; §4.8 queue → A7/B7/C7; §4.9 shutdown → A8/B8 (+C `Dispose`); §5 reconciliation → A9/B9/C9/D1/D2/D3; §6 wiki → F1–F5; §7 versioning → A9/B9/C9/D1/D2; §8 testing → every task's Gate + G1; §9 non-goals → honored (no backend edits; D3 confirms). Covered.

**Placeholder scan:** repetitive per-language steps are compressed but every task names exact files, exact signatures (Interfaces block), exact test intent, and exact gate commands. Novel/load-bearing code (scope async-local, gzip, retry policy, golden test) is specified concretely; where the reference (A) shows the pattern in full, B/C give the language-specific idiom + signatures. No "TBD"/"add error handling"/"similar to Task N, figure it out."

**Type consistency:** scope setters (`setUser/setTag/setTags/setContext/setExtra`), `addBreadcrumb`, `trackTransaction` snake_case wire fields (`duration_ms/http_method/http_status/distinct_id`), and item `type` tags (`error|event|identify|breadcrumb_batch|transaction`) are consistent across A/B/C and match `envelope.rs`. `beforeSend` any-item semantics consistent across A3/B3/C3/D1/D2.
