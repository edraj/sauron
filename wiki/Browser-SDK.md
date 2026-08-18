# Browser SDK — `@edraj/sauron-browser`

Error reporting **+** product analytics **+** performance for the browser, from one
SDK (**v1.6.0**). Source: [`sdks/js`](../sdks/js). SDK header name: `sauron.javascript`.

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/svelte-web`](../examples/svelte-web).

## Install

```bash
npm install @edraj/sauron-browser
```

## What's new in 0.3.0

- **Error items carry more attribution.** An `ErrorItem` now emits `event_id`,
  `message`, `tags`, and `user` when present (all optional — omitted keys are defaulted
  by the backend, so this is additive and non-breaking).
- **`beforeSend` runs on every item.** It is invoked for **every** outgoing item type
  (`error | event | identify | transaction | breadcrumb_batch`), not errors only.

## Init

```ts
import { Sauron } from '@edraj/sauron-browser';

Sauron.init({
  dsn: 'https://<public_key>@<host>/<environment_id>',
  release: 'web@1.4.2',
});
```

You can also import the named functions directly (`import { init, track } from
'@edraj/sauron-browser'`) — the `Sauron` facade and the default export bundle the same
functions.

### `init(options)` options

| Option | Type | Default | Notes |
| --- | --- | --- | --- |
| `dsn` | `string` | *(required)* | `https://<public_key>@<host>/<environment_id>` |
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
| `reset` | `reset(): void` — **call on logout.** Clears the scope user and mints a fresh anonymous id |
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

### Reset on logout — MUST CALL

```ts
Sauron.reset();        // on logout
Sauron.setUser(null);  // equivalent: setUser(null) calls reset() for you
```

The anonymous id is persisted in `localStorage` under `sauron.anon_id` and
survives page loads, tabs and browser restarts. That is what makes the Active
Users report count people rather than page loads — and it is also a durable
first-party identifier stored on the user's terminal, so it is a retention and
consent question for your privacy notice, not just an implementation detail.

Because it is durable, **not calling `reset()` on logout aliases the next
person to the last one**. `identify()` sends the current anonymous id as
`anonymous_id`, and the server records that alias permanently. On a shared or
kiosk browser, the next anonymous visitor reuses the stored `sauron.anon_id`,
and their activity is merged into the previous account server-side, forever.
There is no server-side undo.

As a safety net for exactly that scenario, `identify()` also persists a short
one-way digest (never the id itself) of the last user who identified, under
`localStorage`'s `sauron.last_identified`. If the next `identify()` on this
device is for a DIFFERENT person, the SDK detects the mismatch and mints a
fresh anonymous id (and rotates the session id) before sending — so a
forgotten `reset()` corrupts only that one guest window instead of every one
from then on. This cannot undo an alias already sent under the old id, so
still call `reset()` on logout regardless.

That digest is not a security boundary — it's an unkeyed hash, so over a
possibly low-entropy id (an email address, say) it's a confirmation oracle,
not a secret: anyone with local read access and a guess can verify it
instantly. It exists only so `sauron.last_identified` isn't a second
plaintext copy of your users' ids, not to keep those ids confidential.

`reset()` does NOT clear the device id (`sauron.device_id`) — that identifies
the browser installation, not the person.

See [Active Users](Active-Users.md) for what the identified/guest split means
once these ids reach the backend.

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

SPA navigations are recorded automatically as
`data: { from, to, operation }`, where `operation` is `push` (`pushState`),
`replace` (`replaceState`) or `pop` (`popstate`) — the same vocabulary as the
Flutter SDK's `SauronNavigatorObserver`. Note that a **forward** navigation is
recorded as `pop` too: `history.forward()` fires the same `popstate` as
`history.back()`, so `pop` means "moved through history" rather than
specifically "went back".

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
