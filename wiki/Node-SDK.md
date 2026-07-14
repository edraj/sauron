# Node SDK — `@sauron/node`

Server-side Node/TypeScript SDK. Dispatches product-analytics events and captured
exceptions from your Node backends over a buffered background HTTP transport
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

Requires **Node >= 18** (uses the global `fetch`).

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

## API

| Function | Signature |
| --- | --- |
| `init` | `init(options: InitOptions): SauronClient` |
| `track` | `track(event: string, distinctId: string, properties?: Record<string, unknown>): void` |
| `captureException` | `captureException(error: unknown, options?: CaptureExceptionOptions): void` |
| `captureMessage` | `captureMessage(message: string, level?: Level): void` |
| `identify` | `identify(distinctId: string, traits?: Record<string, unknown>): void` |
| `flush` | `flush(): Promise<void>` |
| `close` | `close(): Promise<void>` — flush, stop the timer, clear the active client |

All dispatch calls are **no-ops** if the SDK is not initialized. `distinctId` is
**required** on `track`. `Level` ∈ `debug | info | warning | error | fatal`.

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

### Identify a user

```ts
identify('user-123', { plan: 'pro' });
```

### Flush / close

```ts
await flush();   // send buffered items now
await close();   // flush, then stop the background timer
```

Call `close()` on shutdown so the buffer drains before the process exits.

## Example

See [`examples/node-server`](../examples/node-server). Run it with:

```bash
cd examples/node-server
npm install
SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
```

Typecheck against the shipped types with `npm run typecheck`. More in
**[Examples](Examples.md)**.
