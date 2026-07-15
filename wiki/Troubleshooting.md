# Troubleshooting

When a signal doesn't show up, or the SDK behaves unexpectedly, this page is the
checklist. It covers all five SDKs (**v0.3.0**) — the shipped surface is described on
each SDK page: **[Browser](Browser-SDK.md)** · **[Flutter](Flutter-SDK.md)** ·
**[Node](Node-SDK.md)** · **[Python](Python-SDK.md)** · **[C#](CSharp-SDK.md)**.

See also: **[Getting Started](Getting-Started.md)** ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)** · **[Dashboard](Dashboard.md)**.

**Turn on debug logging first.** Every SDK logs to stderr/console with a `[sauron]`
prefix when debug is enabled — it tells you about disabled mode, dropped items, auth
failures, and retries. It's the fastest way to see what the transport is doing.

| SDK | Enable debug |
| --- | --- |
| Browser | `Sauron.init({ dsn, debug: true })` |
| Flutter | `o.debug = true` in the `init` callback |
| Node | `init({ dsn, debug: true })` |
| Python | `sauron.init(dsn, debug=True)` |
| C# | `new SauronOptions { Dsn = dsn, Debug = true }` |

---

## Nothing is showing up

Work down this list — the cause is almost always one of the first three.

### 1. The DSN is missing, wrong, or points at another app

Signals are keyed by the **app** the DSN belongs to. A typo, a stale key, or a DSN
copied from a different app sends your data somewhere you aren't looking.

- The DSN is `https://<public_key>@<host>/<project_id>` — no password component. See
  **[Ingest Wire Contract](Ingest-Wire-Contract.md#dsn)**.
- Confirm you're viewing the **same app** in the **[Dashboard](Dashboard.md)** that the
  DSN belongs to.
- A wrong/expired key yields **401/403** from the ingest, which **permanently disables
  the SDK** for the rest of the process (see [Disabled / no-op mode](#disabled--no-op-mode)).
  Enable debug logging — you'll see `[sauron] auth ...`.

### 2. You called a dispatch method *before* `init` (no-op)

**Every** dispatch API (`track`, `captureException`, `captureMessage`, `identify`,
`trackTransaction`, `addBreadcrumb`) is a **no-op until the SDK is initialized**. If a
capture runs before `init` — a module-load-time call, an import side effect, an early
request — it is silently dropped.

- Node: the top-level functions no-op while `getClient()` is `null`.
- Python: no-op while `sauron.get_client()` is `None`.
- C#: no-op while `SauronSdk.Current` is `null`.

Make `init` the first thing your process does, and verify the client exists:

```ts
// Node
import { getClient } from '@sauron/node';
if (!getClient()) console.warn('sauron not initialized');
```

```python
# Python
import sauron
assert sauron.get_client() is not None, "sauron not initialized"
```

```csharp
// C#
if (SauronSdk.Current is null) Console.Error.WriteLine("sauron not initialized");
```

### 3. The process exited before the buffer flushed

All SDKs **buffer** items and flush on a background timer (default every **5 s**) or
once **`maxBatch`** (default **30**) items accumulate. A short-lived server process
(a script, a serverless handler, a CLI) can exit with items still in the buffer — they
never leave the process.

Flush before exit:

| SDK | On shutdown |
| --- | --- |
| Node | `await close();` — flush + stop the timer. Or set `init({ dsn, autoShutdown: true })` to wire `beforeExit`/`SIGTERM`/`SIGINT` to `close()`. |
| Python | `sauron.flush()` then `sauron.close()`. `init` also registers an `atexit` flush automatically. |
| C# | `SauronSdk.Flush();` then `SauronSdk.Close();` |
| Browser | flushes on background/`visibilitychange`; call `await Sauron.flush()` before a hard teardown. |
| Flutter | flushes in the background; `await Sauron.flush()` before exit if needed. |

The Node/C# flush timer is `unref`'d / background, so it **never keeps your process
alive** — a process that has nothing else to do will exit immediately, before the timer
fires. Explicit `close()`/`flush()` is the fix.

### 4. `beforeSend` dropped the item

`beforeSend` runs on **every** outgoing item (`error | event | identify |
transaction`). Returning `null`/`None` **drops** it. If a hook returns nothing on some
code path (e.g. an early `return` with no value), items silently vanish. Make sure the
hook returns the (possibly mutated) item on every path.

```ts
// Node — WRONG: undefined return drops everything
beforeSend: (item) => { redact(item); }        // returns undefined -> dropped
// RIGHT
beforeSend: (item) => { redact(item); return item; }
```

If the hook *throws*, the item is also dropped (logged under debug) — the SDK never lets
your hook crash the app.

### 5. Sampling threw the error away

`sampleRate` (default `1.0` = keep everything) is applied to **handled** captures. With
`sampleRate: 0.1` roughly 90% of `captureException`/`captureMessage` calls are dropped
before enqueue. Uncaught crashes from opt-in [auto-capture](#retry--queue-behavior) are
**always kept** (sampling is bypassed). Set `sampleRate: 1` while debugging.

### 6. Required fields were missing

- `track` requires a non-empty `distinct_id` (C# throws `ArgumentException`; Node/Python
  no-op on an empty id). `identify` requires `distinct_id`.
- `trackTransaction` requires a non-empty `name`.

---

## Disabled / no-op mode

There are **two** distinct "nothing happens" states — don't confuse them:

- **No-op before init** — no client exists yet (covered above). The fix is to call
  `init`.
- **Disabled mode** — `init` ran but the SDK is intentionally inert because the DSN is
  missing/blank. This lets you ship the same code to environments that have no DSN.

**The server SDKs differ here — this is the most common surprise:**

| SDK | Blank/missing DSN | Malformed DSN |
| --- | --- | --- |
| **Python** | **disabled no-op** — `init(dsn=None)` (or `""`) returns `None`, logs `[sauron] no DSN configured; SDK disabled` (debug only), never raises. | raises `DsnError`. |
| **C#** | **disabled no-op** — `Init` succeeds, `Current.Enabled == false`, logs `[sauron] disabled: ...` (debug only). | same disabled no-op (logged), never throws at init. |
| **Flutter** | **disabled no-op** — `Sauron.isEnabled == false`. | disabled no-op. |
| **Node** | **throws** — `init` requires a `{ dsn }` string; a blank/invalid DSN throws (`[sauron] init requires a { dsn }` or a typed `DsnError`). Node has **no silent-disabled mode.** | throws `DsnError`. |

Because Node throws instead of disabling, guard the `init` call yourself when a DSN may
be absent (e.g. local dev):

```ts
// Node — opt out cleanly when no DSN is configured
import { init } from '@sauron/node';
if (process.env.SAURON_DSN) {
  init({ dsn: process.env.SAURON_DSN });
}
// dispatch calls are already no-ops while uninitialized — no other guard needed
```

Check the disabled/enabled state at runtime:

```python
sauron.get_client() is None      # Python: True == disabled
```
```csharp
SauronSdk.Current?.Enabled == true   // C#: false/null == disabled
```
```dart
Sauron.isEnabled                 // Flutter: false == disabled
```

A **hard auth failure (401/403)** also flips an *enabled* SDK into disabled mode
mid-run and clears/stops the queue — it will not retry a bad key. Rotate the DSN and
re-`init`.

---

## Verifying gzip

The transport gzips the request body **only once it exceeds `gzipThresholdBytes`
(default 1024)** and then sets `Content-Encoding: gzip`. Small batches go out
uncompressed — **a single small event is expected to be sent as plain JSON.** That is
usually why "gzip isn't happening": the body was under 1 KiB.

Force the behavior to test it:

| SDK | Compress everything | Disable compression |
| --- | --- | --- |
| Node | `gzipThresholdBytes: 0` | `gzipThresholdBytes: -1` |
| Python | `gzip_threshold_bytes=0` | very large threshold |
| C# | `GzipThresholdBytes = 0` | `GzipThresholdBytes = int.MaxValue` |

**Verify it locally** with the transport's injectable sender seam — no network needed:

```ts
// Node: inspect the outgoing request
import { init, track, flush } from '@sauron/node';
init({
  dsn: 'https://pk@localhost/1',
  gzipThresholdBytes: 0,               // compress even tiny bodies
  fetchImpl: async (_url, init) => {
    console.log('encoding:', init.headers['Content-Encoding']); // -> "gzip"
    console.log('body is bytes:', init.body instanceof Uint8Array); // -> true
    return { status: 200 };
  },
});
track('probe', 'u1', {});
await flush();
```

```python
# Python: the sender receives the (possibly gzipped) body + headers
def sender(url, headers, body):
    print("encoding:", headers.get("Content-Encoding"))  # -> "gzip"
    return 200
sauron.init("https://pk@localhost/1", gzip_threshold_bytes=0, sender=sender)
sauron.track("probe", distinct_id="u1"); sauron.flush()
```

In C#, inject a fake `HttpMessageHandler` (`SauronOptions.HttpMessageHandler`) and read
`request.Content.Headers.ContentEncoding`.

**Verify against a live ingest:** send a batch larger than the threshold and confirm a
**2xx** — the ingest advertises `Content-Encoding: gzip` support (see
**[Ingest Wire Contract](Ingest-Wire-Contract.md#endpoint)**). If a gateway/proxy in
front of the ingest strips or mishandles `Content-Encoding`, raise the threshold to
disable compression as a workaround.

---

## Retry / queue behavior

Understanding the policy explains most "some items arrived, some didn't" reports.

### Retry policy (identical across Node, Python, C#)

| Response | Behavior |
| --- | --- |
| **2xx** | delivered; the batch (and any persisted copies) are dropped. |
| **408 / 413 / 429 / any 5xx** | **retried** with exponential backoff + jitter, capped at **30 s**. On **429** a `Retry-After` header (seconds or HTTP-date) is honored (also capped at 30 s). |
| **network error / timeout** | retried (treated as transient). |
| **400 / 404 and other non-retryable 4xx** | **dropped immediately** (no retry). |
| **401 / 403** | SDK **disables** and stops — a bad key is never retried. |

`maxRetries` defaults to **3** (up to **4** attempts total). After retries are
exhausted the batch is **not silently thrown away**:

- **Node** re-buffers it at the head of the queue for a later flush / next start.
- **C#** keeps the envelope in the queue for later.
- **Python** drops the in-memory copies, but any **disk-persisted** copies remain for
  the next process to recover (see below).

### The queue is byte-bounded (drop-oldest)

Pending items live in an in-memory queue capped at **`maxQueueBytes` (default 1 MiB)**.
During a prolonged outage, when the cap is exceeded the **oldest** items are dropped
first so memory can't grow without bound. If you see gaps during an outage, the cap did
its job — raise `maxQueueBytes` if you can afford the memory, or enable disk persistence.

| SDK | Byte cap | Opt-in disk persistence |
| --- | --- | --- |
| Node | `maxQueueBytes` | `offlineDir: '/var/lib/app/sauron'` |
| Python | `max_queue_bytes` | `offline_path="/var/lib/app/sauron"` |
| C# | `MaxQueueBytes` | `OfflineDir = "/var/lib/app/sauron"` |

**Disk persistence is off by default.** When enabled, pending envelopes are written FIFO
to that directory, **reloaded on the next start**, and deleted once delivered
(**at-least-once** across restarts/crashes). Use it when losing buffered items on a crash
is unacceptable; leave it off on ephemeral/read-only containers.

### Common queue gotchas

- **Items lost on exit** → the process ended before a flush and no `offlineDir` was set.
  See [Nothing is showing up → §3](#3-the-process-exited-before-the-buffer-flushed).
- **Silent drops during an outage** → `maxQueueBytes` was reached. Enable debug logging
  to see the drop/retry messages, then raise the cap or enable disk persistence.
- **A "stuck" SDK sending nothing** → a prior 401/403 disabled it. Rotate the key.

---

## Concurrency / scope-leak pitfalls

Scope (user, tags, contexts, extra, breadcrumbs) is isolated **per async context**, not
per call. Getting this wrong leaks one request's user/tags onto another concurrent
request's errors — the classic bug.

### The leak: mutating the *global* scope from a request handler

Calling `setUser` / `setTag` / `addBreadcrumb` **outside** a pushed scope mutates the
**process-wide global scope**. Under concurrency, request B's error then carries request
A's user/tags. **Fix: push a per-request scope and set request data inside it.**

```ts
// Node — WRONG: leaks across concurrent requests
app.use((req, _res, next) => { setUser({ id: req.userId }); next(); });

// RIGHT: isolate per request (AsyncLocalStorage)
import { runWithAsyncScope, setUser, setTag } from '@sauron/node';
app.use((req, res, next) => {
  runWithAsyncScope(() => {
    setUser({ id: req.userId });
    setTag('route', req.path);
    next();               // downstream captures inherit only THIS request's scope
  });
});
// For a synchronous block that returns a value, use withScope(cb).
```

```python
# Python — isolate per request/task (contextvars)
with sauron.scope():
    sauron.set_user({"id": user_id})
    sauron.set_tag("route", path)
    handle(request)        # captures in here see only this scope
```

```csharp
// C# — isolate per request (AsyncLocal); restores on dispose
using (SauronSdk.PushScope())
{
    SauronSdk.SetUser(new SauronUser { Id = userId });
    SauronSdk.SetTag("route", path);
    await Handle(request);
}
```

### How merging works (so results aren't surprising)

- On capture, tags merge as `{ ...global, ...current }` — **the current (request) scope
  wins** on key collisions; the current user overrides the global user.
- A pushed scope is a **clone taken at push time**. Global changes made *after* the push
  still surface at capture (because global + current are merged then), but the *snapshot*
  of the parent scope is fixed. Set request-specific data on the pushed scope, and only
  truly process-wide data (e.g. `region`, `release`) on the global scope.
- **Breadcrumbs on the global scope attach to every captured error** — put per-request
  breadcrumbs inside the pushed scope.

### Idiom-specific notes

- **Node** (`AsyncLocalStorage`): keep the async work **inside** the `withScope` /
  `runWithAsyncScope` callback so the context spans your `await`s. Context can be lost
  across boundaries that break the async chain (some manual event-emitter callbacks);
  re-establish a scope there if needed.
- **Python** (`contextvars`): each thread and each `copy_context()`-run task gets its own
  scope. Don't share one `Scope` object across threads; use `with sauron.scope():` (or
  `push_scope()`/`pop_scope()`) per unit of work.
- **C#** (`AsyncLocal`): the scope flows into awaited continuations. Don't hold the
  `IDisposable` from `PushScope()` across requests — dispose it (via `using`) at the end
  of the unit of work so the parent scope is restored.

---

## Version checks

All five SDKs ship as **v0.3.0** this release. Every envelope carries the emitting SDK's
identity in `header.sdk.{name, version}` (see
**[Ingest Wire Contract](Ingest-Wire-Contract.md#envelope)**) — if the dashboard shows
an old version arriving, a stale build is still deployed somewhere.

| SDK | Wire `sdk.name` | Read the version in code |
| --- | --- | --- |
| Browser | `sauron.javascript` | `import { SDK_VERSION, SDK_NAME } from '@sauron/browser'` |
| Flutter | `sauron.flutter` | `import 'package:sauron_flutter/sauron_flutter.dart';` → `kSauronSdkVersion` |
| Node | `sauron-node` | `npm ls @sauron/node` (or the `version` in `package.json`) |
| Python | `sauron-python` | `import sauron; sauron.SDK_VERSION` → `'0.3.0'` (and `sauron.SDK_NAME`) |
| C# | `sauron-dotnet` | assembly `<Version>0.3.0</Version>` in `Sauron.csproj` |

If a feature from this release (scope, `addBreadcrumb`, `beforeSend` on every item,
`trackTransaction`, gzip/retry/queue options, opt-in `autoCaptureUnhandled`) is
missing at runtime, you're almost certainly on a **pre-0.3.0** build — pin `0.3.0` and
reinstall. Mismatched versions across services are fine on the wire (the ingest is a
tolerant superset), but the client feature set follows the installed version.
