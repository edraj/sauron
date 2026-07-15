# Browser SDK — `@sauron/browser`

Error reporting **+** product analytics **+** performance for the browser, from one
SDK (**v0.3.0**). Source: [`sdks/js`](../sdks/js). SDK header name: `sauron.javascript`.

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/svelte-web`](../examples/svelte-web).

## Install

```bash
npm install @sauron/browser
```

## What's new in 0.3.0

- **Error items carry more attribution.** An `ErrorItem` now emits `event_id`,
  `message`, `tags`, and `user` when present (all optional — omitted keys are defaulted
  by the backend, so this is additive and non-breaking).
- **`beforeSend` runs on every item.** It is invoked for **every** outgoing item type
  (`error | event | identify | transaction | breadcrumb_batch`), not errors only.

## Init

```ts
import { Sauron } from '@sauron/browser';

Sauron.init({
  dsn: 'https://<public_key>@<host>/<project_id>',
  environment: 'production',
  release: 'web@1.4.2',
});
```

You can also import the named functions directly (`import { init, track } from
'@sauron/browser'`) — the `Sauron` facade and the default export bundle the same
functions.

### `init(options)` options

| Option | Type | Default | Notes |
| --- | --- | --- | --- |
| `dsn` | `string` | *(required)* | `https://<public_key>@<host>/<project_id>` |
| `environment` | `string` | — | e.g. `production` |
| `release` | `string` | — | e.g. `web@1.4.2` |
| `sampleRate` | `number` | `1` | error sample rate in `[0,1]` |
| `maxBreadcrumbs` | `number` | `50` | breadcrumb ring size |
| `beforeSend` | `(item, hint?) => item \| null` | — | drop/mutate any outgoing item |
| `beforeBreadcrumb` | `(crumb, hint?) => crumb \| null` | — | drop/mutate breadcrumbs |
| `transport` | `{ flushIntervalMs?, maxBatch?, maxQueueBytes? }` | `5000` / `30` / `1 MiB` | transport tuning |
| `performance` | `boolean` | `false` | auto-capture navigation/fetch/route transactions |
| `screen` | `string` | — | seed the initial screen name |
| `screenTracking` | `boolean` | `false` | auto-track screen from History navigations |
| `debug` | `boolean` | `false` | verbose diagnostics |

`init` returns a `SauronClient`; `getClient()` returns the active client (or `null`
before init).

## API

| Function | Signature |
| --- | --- |
| `track` | `track(name: string, properties?: Record<string, unknown>): void` |
| `captureException` | `captureException(err: unknown, hint?: Hint): void` |
| `captureMessage` | `captureMessage(message: string, level?: Level, hint?: Hint): void` — default level `info` |
| `identify` | `identify(id: string, traits?: Record<string, unknown>): void` |
| `setUser` | `setUser(user: UserInput): void` — pass `null` to clear |
| `trackTransaction` | `trackTransaction(input: TransactionInput): void` |
| `setScreen` | `setScreen(name: string): void` — emits a `$screen` view on change |
| `getScreen` | `getScreen(): string \| null` |
| `addBreadcrumb` | `addBreadcrumb(breadcrumb: BreadcrumbInput, hint?: Hint): void` |
| `flush` | `flush(timeoutMs?: number): Promise<boolean>` — resolves `false` on timeout |
| `close` | `close(timeoutMs?: number): Promise<boolean>` — flush + tear down, restoring patched globals |

`Level` ∈ `debug | info | warning | error | fatal`.

### Track an event

```ts
Sauron.track('checkout_completed', { cart_value: 42.5, currency: 'USD' });
```

### Capture an exception

```ts
try {
  doWork();
} catch (err) {
  Sauron.captureException(err);
}
```

Uncaught errors and unhandled promise rejections are captured **automatically** once
`init` runs (this is default-on in the browser — no opt-in flag). `captureMessage('cache
warmed', 'info')` sends a bare message.

### Identify a user

```ts
Sauron.identify('u_123', { plan: 'pro' });
// or set the current user on the scope:
Sauron.setUser({ id: 'u_123', email: 'ada@example.com' });
```

The scope's user (from `setUser`) and its tags are stamped onto captured errors (via the
new `user`/`tags` error-item fields). To set a scope tag, reach the client's scope:

```ts
Sauron.getClient()?.getScope().setTag('checkout_step', 'payment');
```

### Breadcrumbs

```ts
Sauron.addBreadcrumb({ type: 'navigation', category: 'route', message: '/settings' });
```

`BreadcrumbInput` fills defaults and stamps a timestamp; crumbs ring-buffer at
`maxBreadcrumbs` (default 50) and attach to errors captured afterwards. A
`beforeBreadcrumb` hook runs first — return `null` to drop the crumb.

### `beforeSend` (any item)

`beforeSend` runs on every outgoing item — scrub PII or drop items. Return the item to
send it, or `null` to drop it:

```ts
Sauron.init({
  dsn,
  beforeSend: (item) => {
    if (item.type === 'event') delete item.properties.email;
    return item; // return null to drop
  },
});
```

### Screen tracking

```ts
Sauron.setScreen('/settings');   // emits a $screen view when the screen changes
Sauron.getScreen();              // -> '/settings'
```

Set `screenTracking: true` in `init` to auto-track the screen from History
navigations. The current screen is stamped onto errors and events.

### Performance transactions

Set `performance: true` to auto-capture navigation, `fetch`, and SPA-route timings, or
record one manually:

```ts
Sauron.trackTransaction({
  name: 'GET /api/users', op: 'http', duration_ms: 128.4,
  http_method: 'GET', http_status: 200, url: '/api/users',
});
```

### Transport: gzip, retry & offline queue

The browser transport handles delivery robustly without extra configuration: large
bodies are gzipped automatically (native `CompressionStream`, falling back to `fflate`)
with `Content-Encoding: gzip`; transient failures (408/429/5xx, network) retry with
backoff and honor `Retry-After`; and pending batches are held in an offline
`localStorage` queue capped by `transport.maxQueueBytes` (default 1 MiB). A `sendBeacon`
path drains the queue on page unload.

### Flush / close

```ts
await Sauron.flush();   // resolves false if the (optional) timeout elapses first
await Sauron.close();   // flush + restore patched globals
```

## Example

See [`examples/svelte-web`](../examples/svelte-web) — a Vite + Svelte 5 single-page
app that exercises the whole surface end-to-end. Run it with:

```bash
cd examples/svelte-web
npm install
npm run dev
```

More in **[Examples](Examples.md)**.
