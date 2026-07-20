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
| `tags` | `Record<string, string>` | — | default scope tags (see [Tags, contexts & extra](#tags-contexts--extra)) |
| `contexts` | `Record<string, Record<string, unknown>>` | — | default scope context blocks |
| `extra` | `Record<string, unknown>` | — | default freeform extra |
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
| `setTag` / `setTags` | `setTag(key: string, value: string): void` · `setTags(tags: Record<string, string>): void` |
| `setContext` | `setContext(name: string, block: Record<string, unknown>): void` — replace a named block |
| `setExtra` | `setExtra(key: string, value: unknown): void` |
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

The scope's user (from `setUser`) and its tags are stamped onto captured errors and
events (via the `user`/`tags` item fields).

### Tags, contexts & extra

Attach your own metadata directly from the top-level API — no need to reach into the
client scope:

```ts
Sauron.setTag('checkout_step', 'payment');          // one filterable tag
Sauron.setTags({ region: 'eu-central', tier: 'pro' });
Sauron.setContext('cart', { item_count: 3, total: 42.5 }); // a named structured block
Sauron.setExtra('experiment_bucket', 'B');          // a loose one-off value
```

A value set on the scope is lifted onto **every later error/event**. You can also seed
defaults at `init` (`tags` / `contexts` / `extra`), or pass them for a single call:

```ts
Sauron.captureException(err, {
  tags: { severity: 'high' },
  contexts: { order: { id: 'ord_1001', items: 3 } },
});
```

**Tags** are a flat `key → value` map (indexed for filtering); **contexts** are named
structured blocks; **extra** is loose values — all developer-set, and distinct from the
SDK's machine-collected `context` (device/OS/browser). See
**[Best Practices §4](Best-Practices.md)** for when to use which, the
**[Dashboard](Dashboard.md)** for where they appear, and **[Search](Search.md)** to
filter by them.

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
