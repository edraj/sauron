# Browser SDK — `@sauron/browser`

Error reporting **+** product analytics **+** performance for the browser, from one
SDK. Source: [`sdks/js`](../sdks/js). SDK header name: `sauron.javascript`.

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/svelte-web`](../examples/svelte-web).

## Install

```bash
npm install @sauron/browser
```

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
| `beforeSend` | `(item, hint?) => item \| null` | — | drop/mutate items |
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

Uncaught errors and unhandled promise rejections are captured automatically once
`init` runs. `captureMessage('cache warmed', 'info')` sends a bare message.

### Identify a user

```ts
Sauron.identify('u_123', { plan: 'pro' });
// or set the current user on the scope:
Sauron.setUser({ id: 'u_123', email: 'ada@example.com' });
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

### Flush

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
