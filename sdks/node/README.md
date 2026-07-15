# @sauron/node

Server-side Node/TypeScript SDK for [Sauron](https://sauron.dev) — dispatch
product-analytics events and captured exceptions from your Node backends.

This is the **server-side** SDK (no browser/DOM/auto-instrumentation). For the
browser, use `@sauron/browser` (`sdks/js`).

## Install

```bash
npm install @sauron/node
```

Requires Node >= 18 (uses the global `fetch` and `zlib`).

## Usage

```ts
import {
  init,
  track,
  captureException,
  captureMessage,
  identify,
  trackTransaction,
  addBreadcrumb,
  withScope,
  setUser,
  setTag,
  flush,
  close,
} from '@sauron/node';

init({
  dsn: 'https://<public_key>@<host>/<project_id>',
  environment: 'production',
  release: '1.4.2',
  // Opt-in (both default off):
  autoCaptureUnhandled: true, // capture uncaughtException / unhandledRejection
  autoShutdown: true, // flush on beforeExit / SIGTERM / SIGINT
});

// Product analytics — distinctId is required.
track('order_completed', 'user-123', { total: 42.5, currency: 'USD' });

// Exceptions
try {
  doWork();
} catch (err) {
  captureException(err, { user: { id: 'user-123' }, tags: { area: 'checkout' } });
}

captureMessage('cache warm-up finished', 'info');
identify('user-123', { plan: 'pro' });

// On shutdown
await close(); // flushes then stops the background timer
```

### Per-request scope

Isolate user/tags/breadcrumbs per request with `withScope` — backed by
`AsyncLocalStorage`, so concurrent requests never leak state into each other:

```ts
app.use((req, res, next) => {
  withScope(() => {
    setUser({ id: req.userId });
    setTag('route', req.route.path);
    addBreadcrumb({ category: 'http', message: `${req.method} ${req.url}` });
    next();
  });
});
```

`captureException` automatically attaches the active scope's user, tags and
breadcrumb trail. An optional `fingerprint` override is honored verbatim by the
backend.

### Transactions

```ts
trackTransaction({
  name: 'GET /api/users',
  op: 'http',
  duration_ms: 12.5,
  http_method: 'GET',
  http_status: 200,
}); // distinct_id falls back to the scoped user's id when omitted
```

## API

| Function | Description |
| --- | --- |
| `init(options)` | Create the global client. Throws `DsnError` on an invalid DSN. |
| `track(event, distinctId, properties?)` | Capture an analytics event. |
| `captureException(error, options?)` | Capture a native `Error` (attaches scope). |
| `captureMessage(message, level?)` | Capture a bare message. |
| `identify(distinctId, traits?)` | Associate traits with a user. |
| `trackTransaction(input)` | Emit a performance transaction. |
| `addBreadcrumb(crumb)` | Add a breadcrumb to the active scope (runs `beforeBreadcrumb`). |
| `setUser / setTag / setTags / setContext / setExtra` | Mutate the active scope. |
| `withScope(cb)` / `configureScope(cb)` | Run with an isolated child scope / mutate the current one. |
| `installShutdownHooks(client)` | Wire `beforeExit`/`SIGTERM`/`SIGINT` to `close()`. |
| `flush()` | Send buffered items immediately. |
| `close()` | Flush, stop the timer, and remove any installed process hooks. |

Every dispatch function is a no-op before `init` / when the SDK is disabled.

### `init` options

`environment`, `release`, `sampleRate`, `flushInterval`, `maxBatch`,
`maxBreadcrumbs` (default 100), `gzipThresholdBytes` (default 1024),
`maxQueueBytes` (default 1 MiB), `offlineDir` (opt-in FIFO disk persistence),
`maxRetries` (default 3), `autoCaptureUnhandled` (default off),
`autoShutdown` (default off), `beforeSend(item)`, `beforeBreadcrumb(crumb)`.

## Transport

Items buffer in a byte-bounded in-memory queue (drop-oldest past `maxQueueBytes`,
optionally persisted to `offlineDir` for at-least-once delivery across restarts)
and flush every `flushInterval` ms (default 5000) or once `maxBatch` items
(default 30) accumulate. The flush timer is `unref`'d so it never keeps your
process alive. Each flush POSTs one envelope to
`{proto}://{host}/api/{project_id}/envelope` with an `X-Sauron-Key` header,
gzipping the body once it crosses `gzipThresholdBytes` (`Content-Encoding: gzip`).
Transient failures (408/413/429/5xx, network) retry with exponential backoff +
jitter honoring `Retry-After`; 400/401/403/404 drop without retry.

## Development

```bash
npm install
npm run build
npm test
```
