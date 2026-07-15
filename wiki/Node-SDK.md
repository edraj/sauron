# Node SDK — `@sauron/node`

Server-side Node/TypeScript SDK (**v0.3.0**). Dispatches product-analytics events and
captured exceptions from your Node backends over a buffered background HTTP transport
(Node's global `fetch`). **No browser/DOM/auto-instrumentation** — for the browser use
the **[Browser SDK](Browser-SDK.md)** (`@sauron/browser`). Source:
[`sdks/node`](../sdks/node). SDK header name: `sauron-node`.

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/node-server`](../examples/node-server).

## Install

```bash
npm install @sauron/node
```

Requires **Node >= 18** (uses the global `fetch` and `zlib`).

## Init

```ts
import { init, track, captureException, identify, flush, close } from '@sauron/node';

init({
  dsn: 'https://<public_key>@<host>/<project_id>',
  environment: 'production',
  release: '1.4.2',
});
```

`init` returns the global `SauronClient` and throws a typed `DsnError` on a
clearly-invalid DSN. `getClient()` returns the client created by the most recent
`init` (or `null`). Transport tuning: items buffer and flush every `flushInterval` ms
(default `5000`) or once `maxBatch` items (default `30`) accumulate; the timer is
`unref`'d so it never keeps your process alive.

### `init(options)` options

| Option | Type | Default | Notes |
| --- | --- | --- | --- |
| `dsn` | `string` | *(required)* | `https://<public_key>@<host>/<project_id>` |
| `environment` | `string` | `"production"` | |
| `release` | `string \| null` | `null` | |
| `sampleRate` | `number` | `1` | error sample rate in `[0,1]` |
| `flushInterval` | `number` | `5000` | background flush interval, ms |
| `maxBatch` | `number` | `30` | eager flush at this many buffered items |
| `maxBreadcrumbs` | `number` | `100` | breadcrumb ring size on the global scope |
| `beforeSend` | `(item, hint?) => item \| null` | — | drop/mutate any outgoing item |
| `beforeBreadcrumb` | `(crumb, hint?) => crumb \| null` | — | drop/mutate breadcrumbs |
| `gzipThresholdBytes` | `number` | `1024` | gzip the body once it exceeds this size |
| `maxQueueBytes` | `number` | `1048576` | drop-oldest byte cap for the in-memory send buffer |
| `offlineDir` | `string` | — | opt-in dir for FIFO disk persistence of pending envelopes |
| `maxRetries` | `number` | `3` | retries after the first attempt for transient failures |
| `autoCaptureUnhandled` | `boolean` | `false` | opt-in uncaught-exception / rejection capture |
| `autoShutdown` | `boolean` | `false` | opt-in `beforeExit`/`SIGTERM`/`SIGINT` → `close()` |
| `debug` | `boolean` | `false` | log transport diagnostics to stderr |

`Level` ∈ `debug | info | warning | error | fatal`.

## API

| Function | Signature |
| --- | --- |
| `init` | `init(options: InitOptions): SauronClient` |
| `track` | `track(event: string, distinctId: string, properties?: Record<string, unknown>): void` |
| `captureException` | `captureException(error: unknown, options?: CaptureExceptionOptions): void` |
| `captureMessage` | `captureMessage(message: string, level?: Level): void` |
| `identify` | `identify(distinctId: string, traits?: Record<string, unknown>): void` |
| `trackTransaction` | `trackTransaction(input: TransactionInput): void` |
| `addBreadcrumb` | `addBreadcrumb(crumb: BreadcrumbInput): void` |
| `setUser` | `setUser(user: User \| null): void` |
| `setTag` | `setTag(key: string, value: string): void` |
| `setTags` | `setTags(tags: Record<string, string>): void` |
| `setContext` | `setContext(key: string, context: unknown): void` |
| `setExtra` | `setExtra(key: string, value: unknown): void` |
| `withScope` | `withScope<T>(cb: (scope: Scope) => T): T` |
| `runWithAsyncScope` | `runWithAsyncScope<T>(cb: () => T): T` |
| `configureScope` | `configureScope(cb: (scope: Scope) => void): void` |
| `installShutdownHooks` | `installShutdownHooks(client: SauronClient): () => void` |
| `installAutoCapture` | `installAutoCapture(client: SauronClient): () => void` |
| `flush` | `flush(): Promise<void>` |
| `close` | `close(): Promise<void>` — flush, stop the timer, clear the active client |

All dispatch calls are **no-ops** if the SDK is not initialized. `distinctId` is
**required** on `track`.

### Track an event

```ts
track('order_completed', 'user-123', { total: 42.5, currency: 'USD' });
```

### Capture an exception

```ts
try {
  doWork();
} catch (err) {
  captureException(err, { user: { id: 'user-123' }, tags: { area: 'checkout' } });
}

captureMessage('cache warm-up finished', 'info');
```

`CaptureExceptionOptions` = `{ user?, level?, tags?, handled?, fingerprint? }`. A
supplied `fingerprint` (a `string[]`) is honored verbatim by the backend for grouping.

### Identify a user

```ts
identify('user-123', { plan: 'pro' });
```

## Scope, tags & context

A single **global scope** holds process-wide user/tags/context/breadcrumbs. The
top-level setters mutate the *active* scope (the global one outside a scoped block):

```ts
import { setUser, setTag, setTags, setContext, setExtra } from '@sauron/node';

setUser({ id: 'user-123', email: 'ada@example.com' }); // pass null to clear
setTag('region', 'eu-west-1');
setTags({ tier: 'pro', shard: '7' });
setContext('order', { id: 'ord_1001', items: 3 });
setExtra('cacheHit', false);
```

Scope tags/user/breadcrumbs are merged onto every captured error (per-call `tags`/`user`
passed to `captureException` win over scope values).

### Per-request isolation with `withScope`

`withScope` layers an **isolated child scope** over the current one for the duration of
a callback, backed by `AsyncLocalStorage` so concurrent requests never leak state into
each other. It returns whatever the callback returns (await it if the callback is async):

```ts
import { withScope } from '@sauron/node';

await withScope(async (scope) => {
  scope.setUser({ id: req.userId });
  scope.setTag('route', 'POST /checkout');
  scope.addBreadcrumb({ category: 'auth', message: 'token verified' });
  // any captureException in here inherits this scope's user/tags/breadcrumbs
  await handle(req);
});
```

`runWithAsyncScope(cb)` is the same without the scope argument; `configureScope(cb)`
mutates the active scope in place.

## Breadcrumbs

```ts
import { addBreadcrumb } from '@sauron/node';

addBreadcrumb({ category: 'db', message: 'SELECT users', level: 'info', data: { ms: 4 } });
```

`BreadcrumbInput` = `{ type?, category?, message?, level?, data? }`. Missing fields are
defaulted and an ISO `timestamp` is stamped. The crumb lands on the **active** scope
and attaches to errors captured afterwards (ring-buffered at `maxBreadcrumbs`, default
100). A `beforeBreadcrumb` hook runs first — return `null` to drop the crumb:

```ts
init({
  dsn,
  beforeBreadcrumb: (crumb) => (crumb.category === 'noisy' ? null : crumb),
});
```

## `beforeSend` (any item)

`beforeSend` runs on **every** outgoing item (`error | event | identify | transaction`)
at the single enqueue chokepoint — the place to scrub PII or drop items. Return the
(possibly mutated) item to send it, or `null` to drop it:

```ts
init({
  dsn,
  beforeSend: (item) => {
    if (item.type === 'event' && item.properties?.email) {
      item.properties.email = '[redacted]';
    }
    return item; // return null to drop
  },
});
```

## Performance transactions

```ts
import { trackTransaction } from '@sauron/node';

const start = performance.now();
// ... handle request ...
trackTransaction({
  name: 'GET /api/users', op: 'http', duration_ms: performance.now() - start,
  http_method: 'GET', http_status: 200, url: '/api/users',
});
```

`TransactionInput` = `{ name, op?, duration_ms, status?, http_method?, http_status?,
url?, distinct_id? }`. `op` defaults to `'custom'`; wire fields are snake_case; optional
fields are omitted from the JSON when absent. `distinct_id` falls back to the scoped
user's id when omitted.

## Gzip, retry & the offline queue

- **Gzip** — the request body is gzipped once it exceeds `gzipThresholdBytes` (default
  1024), with `Content-Encoding: gzip`; smaller bodies go out uncompressed. Uses Node's
  built-in `zlib` (no runtime dependency).
- **Retry** — transient failures (408/413/429/5xx and network errors) retry with
  exponential backoff + jitter (capped at 30 s), honoring `Retry-After` on 429, up to
  `maxRetries` (default 3); after that the batch is re-buffered for a later flush. 400/404
  drop the batch; **401/403 disable the SDK**. Non-retryable statuses never retry.
- **Queue** — items buffer in a byte-bounded queue (`maxQueueBytes`, default 1 MiB,
  drop-oldest). Set `offlineDir` to persist pending envelopes FIFO to disk (reloaded on
  the next start, deleted on delivery) for at-least-once delivery across restarts.

## Auto-capture & graceful shutdown

Both are **opt-in** and OFF by default:

```ts
init({ dsn, autoCaptureUnhandled: true, autoShutdown: true });
```

- `autoCaptureUnhandled` registers `uncaughtException` / `unhandledRejection` handlers
  that capture with `mechanism.handled = false`, flush, and **preserve** Node's default
  crash/exit behavior (it never swallows the process).
- `autoShutdown` wires `beforeExit` / `SIGTERM` / `SIGINT` to `close()` so the buffer
  drains on shutdown. You can also install these manually with
  `installShutdownHooks(client)` / `installAutoCapture(client)` (both return an
  uninstaller and are idempotent per client).

### Flush / close

```ts
await flush();   // send buffered items now
await close();   // flush, stop the background timer, remove opt-in hooks
```

Call `close()` on shutdown so the buffer drains before the process exits (or use
`autoShutdown`).

## Example

See [`examples/node-server`](../examples/node-server). Run it with:

```bash
cd examples/node-server
npm install
SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
```

Typecheck against the shipped types with `npm run typecheck`. More in
**[Examples](Examples.md)**.
