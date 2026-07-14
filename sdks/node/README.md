# @sauron/node

Server-side Node/TypeScript SDK for [Sauron](https://sauron.dev) — dispatch
product-analytics events and captured exceptions from your Node backends.

This is the **server-side** SDK (no browser/DOM/auto-instrumentation). For the
browser, use `@sauron/browser` (`sdks/js`).

## Install

```bash
npm install @sauron/node
```

Requires Node >= 18 (uses the global `fetch`).

## Usage

```ts
import { init, track, captureException, captureMessage, identify, flush, close } from '@sauron/node';

init({
  dsn: 'https://<public_key>@<host>/<project_id>',
  environment: 'production',
  release: '1.4.2',
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

## API

| Function | Description |
| --- | --- |
| `init(options)` | Create the global client. Throws `DsnError` on an invalid DSN. |
| `track(event, distinctId, properties?)` | Capture an analytics event. |
| `captureException(error, options?)` | Capture a native `Error`. |
| `captureMessage(message, level?)` | Capture a bare message. |
| `identify(distinctId, traits?)` | Associate traits with a user. |
| `flush()` | Send buffered items immediately. |
| `close()` | Flush then stop the background flush timer. |

## Transport

Items buffer in memory and flush every `flushInterval` ms (default 5000) or once
`maxBatch` items (default 30) accumulate. The flush timer is `unref`'d so it
never keeps your process alive. Each flush POSTs one envelope to
`{proto}://{host}/api/{project_id}/envelope` with an `X-Sauron-Key` header.

## Development

```bash
npm install
npm run build
npm test
```
