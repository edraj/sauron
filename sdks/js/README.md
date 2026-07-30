# @edraj/sauron-browser

Client-side SDK for **Sauron** — error reporting and product analytics for the
browser in one small package. It runs in the page (or any browser-like host) and
posts a canonical JSON envelope to the Sauron ingest gateway. For a Node.js
process — an API server, a worker, a CLI — use the server SDK
[`@edraj/sauron-node`](../node) instead; this package assumes browser globals
(`window`, `document`, `localStorage`) and ships a public, write-only DSN key.

- Auto-instruments `window.onerror`, `onunhandledrejection`, `console`, DOM
  clicks, `fetch`, `XMLHttpRequest`, and SPA History navigations out of the box.
- Opt-in performance transactions (navigation timing, per-`fetch` HTTP spans,
  SPA route spans) and opt-in screen tracking.
- Batches, gzips, retries with jitter, and parks failed envelopes in a
  `localStorage` queue that drains on the next page load or `online` event.
- One runtime dependency (`fflate`, lazily imported only as a gzip fallback).
- Ships ESM + CJS + type declarations, `sideEffects: false`, tree-shakeable.

## Install

```bash
npm install @edraj/sauron-browser
```

Node >= 18 is required for the build/test tooling (`engines.node`). The shipped
bundle targets ES2020 and needs no polyfills in evergreen browsers.

## Quick start

```ts
import { Sauron } from '@edraj/sauron-browser';

Sauron.init({
  dsn: 'https://pk_test@ingest.example.com/42',
  release: 'web@1.4.2',
});

Sauron.identify('u_123', { plan: 'pro' });
Sauron.track('checkout_completed', { cart_value: 42.5 });

try {
  doRiskyThing();
} catch (err) {
  Sauron.captureException(err);
}

// Optional: force delivery now instead of waiting for the 5 s flush tick.
await Sauron.flush(2000);
```

Uncaught errors and unhandled rejections need no code at all — `init()` installs
the global handlers.

## Configuration

`init(options)` takes an `InitOptions` object. Only `dsn` is required; anything
missing falls back to the default below (resolved in `resolveOptions()`).

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `string` | — **(required)** | `https://<public_key>@<host>/<environment_id>`. A non-string or empty value throws `Error`; a malformed URL throws `DsnError`. |
| `release` | `string` | `null` | Stamped on `header.release`; the part after the last `@` also becomes `context.app.version` (`web@1.4.2` → `1.4.2`). |
| `sampleRate` | `number` | `1` | Fraction of **error items** sent, clamped into `[0, 1]`. Applies to `captureException`, `captureMessage` and the global handlers only — events, identifies and transactions are never sampled. |
| `maxBreadcrumbs` | `number` | `50` | Ring-buffer size; oldest entries are evicted first. Negative values are treated as `0`, which disables breadcrumbs entirely. |
| `beforeSend` | `(item: EnvelopeItem, hint?: Hint) => EnvelopeItem \| null` | `undefined` | Runs on **every** item type just before the transport. Return `null` to drop. If it throws, the original item is sent and a warning is logged in `debug` mode. |
| `beforeBreadcrumb` | `(breadcrumb: Breadcrumb, hint?: Hint) => Breadcrumb \| null` | `undefined` | Runs on every breadcrumb before it enters the buffer. Return `null` to drop. Throwing keeps the original. |
| `transport` | `TransportOptions` | see below | Batching / queue tuning. |
| `performance` | `boolean` | `false` | Opt-in performance auto-capture (see [Automatic instrumentation](#automatic-instrumentation)). Manual `trackTransaction()` works regardless. |
| `screen` | `string` | `undefined` | Seeds the initial screen name. Seeding does **not** emit a `$screen` event — only a later `setScreen()` change does. |
| `screenTracking` | `boolean` | `false` | Opt-in: set the screen to the new path on every SPA History navigation (which emits `$screen`). `setScreen()` works regardless. |
| `tags` | `Record<string, string>` | `{}` | Default tags seeded into the global scope. |
| `contexts` | `Record<string, Record<string, unknown>>` | `{}` | Default named context blocks seeded into the global scope. |
| `extra` | `Record<string, unknown>` | `{}` | Default freeform values seeded into the global scope. |
| `debug` | `boolean` | `false` | Log SDK diagnostics to `console` with a `[sauron]` prefix. |

`TransportOptions`:

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `flushIntervalMs` | `number` | `5000` | Periodic flush cadence. A value `<= 0` disables the timer (you must call `flush()` yourself). |
| `maxBatch` | `number` | `30` | Items per envelope before an eager flush. Clamped into `[1, 1000]` — 1000 is the server's per-envelope item limit. |
| `maxQueueBytes` | `number` | `1048576` | Byte cap on the offline `localStorage` queue (1 MiB). Negative values are treated as `0`. |

Every option set at once:

```ts
import { Sauron } from '@edraj/sauron-browser';

Sauron.init({
  dsn: 'https://pk_test@ingest.example.com/42',
  release: 'web@1.4.2',
  sampleRate: 0.5,
  maxBreadcrumbs: 100,
  beforeSend(item, hint) {
    if (item.type === 'error' && item.exception.value?.includes('token=')) {
      return null; // PII escape hatch
    }
    return item;
  },
  beforeBreadcrumb(crumb) {
    return crumb.category === 'console' ? null : crumb;
  },
  transport: {
    flushIntervalMs: 5000,
    maxBatch: 30,
    maxQueueBytes: 1048576,
  },
  performance: true,
  screen: '/',
  screenTracking: true,
  tags: { tier: 'free' },
  contexts: { deploy: { region: 'eu-west-1' } },
  extra: { build: 'ci-42' },
  debug: true,
});
```

## API reference

Everything is exported both as a named function and as a member of the `Sauron`
facade (also the default export). The two are the same function:

```ts
import { Sauron } from '@edraj/sauron-browser';          // facade
import Sauron from '@edraj/sauron-browser';              // default export
import { init, captureException } from '@edraj/sauron-browser'; // named
```

The facade carries `init`, `captureException`, `captureMessage`, `track`,
`trackTransaction`, `identify`, `addBreadcrumb`, `setUser`, `setTag`, `setTags`,
`setContext`, `setExtra`, `setScreen`, `getScreen`, `startWorkflow`,
`endWorkflow`, `cancelWorkflow`, `getWorkflow`, `flush`, `close` and
`getClient`.

Before `init()` every capture, analytics and scope function is a silent no-op,
`getScreen()`/`getWorkflow()` return `null`, `startWorkflow`/`endWorkflow`/
`cancelWorkflow` resolve to `{ status: 'disabled' }`, and `flush()`/`close()`
resolve to `false`. Nothing throws. After `close()` the capture and scope
functions stay no-ops (the client is disabled) and the screen and active
workflow are reset to `null`.

The same disabled state is also reached **automatically, mid-session**, the
moment the gateway answers a delivery with `401`/`403` (a revoked or invalid
DSN key) — no call to `close()`/`disable()` required. Check
[`isEnabled()`](#sauronclient) rather than assuming the SDK is live just
because you never explicitly disabled it.

### `init(options)`

```ts
function init(options: InitOptions): SauronClient
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `options` | `InitOptions` | — (required) | See [Configuration](#configuration). |

Resolves defaults, parses the DSN, seeds `tags`/`contexts`/`extra` into the
global scope, installs the integrations, starts the flush timer and drains the
offline queue. Returns the live `SauronClient`.

Idempotent: a second `init()` tears the previous client down first (restoring
every patched global, clearing the current screen) before installing a fresh
one. Throws `Error` when `dsn` is missing or not a string, and `DsnError` when
the DSN itself is malformed.

```ts
const client = Sauron.init({ dsn: 'https://pk_test@localhost:8081/1' });
client.options.release;     // null
client.dsn.projectId;       // '1'
```

### `captureException(err, hint?)`

```ts
function captureException(err: unknown, hint?: Hint): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `err` | `unknown` | — (required) | An `Error`, an error-like object (`name` + string `message`), a string, any object, or a primitive. Non-errors are reduced to `{type, value}` with an empty stack trace. |
| `hint` | `Hint` | `undefined` | Per-call overrides, also forwarded to `beforeSend`. |

Recognized `hint` keys:

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `level` | `Level` | `'error'` | `'debug' \| 'info' \| 'warning' \| 'error' \| 'fatal'`. |
| `mechanism` | `Mechanism` | `{ type: 'generic', handled: true }` | How the error reached the SDK. |
| `fingerprint` | `string[] \| null` | `null` | Overrides server-side grouping. |
| `screen` | `string` | current screen | Screen stamped on this item. |
| `event_id` | `string` | fresh UUID v4 | Correlation id for the report. |
| `message` | `string` | `undefined` | Human summary alongside the exception. |
| `tags` | `Record<string, string>` | `undefined` | Merged over scope tags (this call only). |
| `contexts` | `Record<string, Record<string, unknown>>` | `undefined` | Merged over scope contexts (this call only). |
| `extra` | `Record<string, unknown>` | `undefined` | Merged over scope extra (this call only). |

Any other key is passed through to `beforeSend` untouched. `originalException`
is always set to `err` on the hint the SDK hands to `beforeSend`. Returns
`void`; the item is buffered, not sent synchronously.

```ts
try {
  await placeOrder(orderId);
} catch (err) {
  Sauron.captureException(err, {
    level: 'fatal',
    fingerprint: ['checkout', 'place-order'],
    tags: { flow: 'checkout' },
    contexts: { order: { id: orderId } },
    extra: { retry_count: 2 },
  });
}
```

### `captureMessage(message, level?, hint?)`

```ts
function captureMessage(message: string, level?: Level, hint?: Hint): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `message` | `string` | — (required) | Becomes `exception.value`; `exception.type` is `null`. |
| `level` | `Level` | `'info'` | Severity. |
| `hint` | `Hint` | `undefined` | Only `fingerprint`, `event_id`, `message`, `tags`, `contexts` and `extra` are read here — unlike `captureException`, `hint.level`, `hint.mechanism` and `hint.screen` are ignored. |

Emits an error item with mechanism `{ type: 'message', handled: true }`, an
empty stack trace, the current breadcrumb trail and the current screen. Counts
against `sampleRate` like any other error item. Returns `void`.

```ts
Sauron.captureMessage('payment provider returned a soft decline', 'warning', {
  tags: { provider: 'stripe' },
});
```

### `track(name, properties?, options?)`

```ts
function track(
  name: string,
  properties?: Record<string, unknown>,
  options?: TrackOptions,
): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — (required) | Event name. The SDK itself emits one reserved name, `$screen`. |
| `properties` | `Record<string, unknown>` | `{}` | Event properties, sent verbatim. |
| `options` | `TrackOptions` | `{}` | Per-call metadata, see below. |

`TrackOptions` (extends `CaptureOptions`):

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `tags` | `Record<string, string>` | `undefined` | Merged over scope tags. |
| `contexts` | `Record<string, Record<string, unknown>>` | `undefined` | Merged over scope contexts (per block name). |
| `extra` | `Record<string, unknown>` | `undefined` | Merged over scope extra. |
| `screen` | `string` | current screen | Screen stamped on this event only; does not change the current screen. |

The event carries `distinct_id` (the identified user id, else a lazily minted
`anon_<uuid>`), the session id and the screen. Events are never sampled.
Returns `void`.

```ts
Sauron.track('checkout_completed', { cart_value: 42.5, currency: 'EUR' }, {
  tags: { experiment: 'new-cart' },
  contexts: { cart: { items: 3 } },
  extra: { coupon: 'SUMMER' },
  screen: '/checkout/confirm',
});
```

### `identify(id, traits?)`

```ts
function identify(id: string, traits?: Record<string, unknown>): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `id` | `string` | — (required) | The distinct id (your user id). |
| `traits` | `Record<string, unknown>` | `{}` | Traits stored on the scope user and sent on the identify item. |

Sets the scope user to `{ id, traits }` and emits an identify item whose
`anonymous_id` is the previously minted anonymous id, or `null` when the session
never needed one. Note that this replaces the whole scope user — an `email` set
earlier via `setUser()` is cleared; call `setUser()` after `identify()` if you
need it. Returns `void`.

```ts
Sauron.identify('u_123', { plan: 'pro', signup_month: '2026-03' });
```

### `trackTransaction(input)`

```ts
function trackTransaction(input: TransactionInput): void
```

| Field of `input` | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — (required) | Transaction name, e.g. `GET /api/orders`. |
| `durationMs` | `number` | — (required) | Wall-clock span in milliseconds. |
| `op` | `string` | `'custom'` | One of `'navigation' \| 'http' \| 'resource' \| 'screen_load' \| 'custom'`. Anything else is coerced to `'custom'`. |
| `status` | `string \| null` | `null` | Free-form outcome, e.g. `'ok'` / `'error'`. |
| `httpMethod` | `string \| null` | `null` | For `http` ops. |
| `httpStatus` | `number \| null` | `null` | For `http` ops. |
| `url` | `string \| null` | `null` | For `http` ops. |

The item is stamped with the current distinct id, session id and timestamp.
Never sampled. Returns `void`.

```ts
const started = performance.now();
const res = await fetch('/api/orders');
Sauron.trackTransaction({
  name: 'GET /api/orders',
  op: 'http',
  durationMs: performance.now() - started,
  status: res.ok ? 'ok' : 'error',
  httpMethod: 'GET',
  httpStatus: res.status,
  url: '/api/orders',
});
```

### `setScreen(name)`

```ts
function setScreen(name: string): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — (required) | The new screen/route name. |

Sets the current screen. On an actual change it also emits a `$screen` event
with `properties: { screen: name }` so dwell time can be computed server-side;
calling it again with the same name is a no-op. The current screen is stamped on
every subsequent event and error item. Returns `void`.

```ts
router.afterEach((to) => Sauron.setScreen(to.path));
```

### `getScreen()`

```ts
function getScreen(): string | null
```

Returns the current screen name, or `null` when none was ever set (and after
`close()`, which resets it).

```ts
if (Sauron.getScreen() !== '/checkout') Sauron.setScreen('/checkout');
```

### `startWorkflow(name, options?)`

```ts
function startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — (required) | Workflow name. Trimmed; rejected if empty after trimming or longer than 120 characters. |
| `options.force` | `boolean` | `false` | Replace an already-active workflow instead of rejecting the call. |

Starts a named, explicitly-bounded span of activity — e.g. `checkout`,
`onboarding` — and mints a fresh **client-generated UUID** as its
`workflowId`/wire `workflow_id`. While a workflow is active, every subsequent
`track`, `captureException`, `captureMessage` and `trackTransaction` call is
additionally stamped with `workflow_id` + `workflow_name`, alongside whatever
else it already carries. `startWorkflow` itself emits a reserved
`$workflow_start` event, stamped with the *new* workflow.

Workflows are entirely optional: an app that never calls `startWorkflow`
behaves exactly as before — no `workflow_id`/`workflow_name` fields are ever
added to any item.

Returns a `WorkflowResult`:

| `status` | Meaning |
| --- | --- |
| `'ok'` | Started (or replaced, with `force`). `workflowId` is the new id. |
| `'already_active'` | Another workflow is already active and `force` was not set. Nothing changed. |
| `'invalid_name'` | `name` was empty after trimming, or over 120 characters. Nothing changed. |
| `'disabled'` | Called before `init()`, after the client was closed/disabled, or after the transport auto-disabled itself on a `401`/`403` — also returned if an unexpected internal error occurred. Nothing changed. |

With `force: true`, the previously-active workflow is closed first — emitting
`$workflow_cancel` for it with `reason: 'superseded'` — and then the new one
starts. Without `force`, an active workflow simply blocks the call (logged as
a warning in `debug` mode). Telemetry never throws: every precondition failure
returns a status instead.

`'disabled'` always means *nothing changed*, so it is never worth retrying
blindly. If the workflow started but its `$workflow_start` event could not be
delivered, you still get `'ok'` and a `workflowId` — the workflow is live and
stamping is active, and the server materializes the workflow from the first
stamped event it receives regardless.

```ts
const result = Sauron.startWorkflow('checkout');
if (result.status === 'ok') {
  console.log('workflow id', result.workflowId);
}

// Force-replace whatever workflow (if any) is currently active:
Sauron.startWorkflow('checkout', { force: true });
```

### `endWorkflow(name?)`

```ts
function endWorkflow(name?: string): WorkflowResult
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | current workflow | If given, must match the active workflow's name or the call is rejected. |

Ends the active workflow: emits `$workflow_end` carrying `duration_ms` (the
time since `startWorkflow`), then clears the active workflow.

| `status` | Meaning |
| --- | --- |
| `'ok'` | Ended. `workflowId` is the id that was closed. |
| `'not_active'` | No workflow is active. Nothing changed. |
| `'name_mismatch'` | `name` was given but does not match the active workflow. Nothing changed. |
| `'disabled'` | Called before `init()`, after the client was closed/disabled, or after the transport auto-disabled itself on a `401`/`403` — also returned if an unexpected internal error occurred. Nothing changed. |

A `name` that is itself malformed — empty, whitespace-only, or over 120
characters — reports `'name_mismatch'`, not `'invalid_name'`: it cannot match
the active workflow, and the call named a workflow that is not the active one.
`'invalid_name'` is reserved for `startWorkflow`, where the name is the thing
being created rather than a guard on which workflow to close.

`'ok'` always means the workflow really is closed locally, even in the rare
case where the `$workflow_end` event itself could not be delivered — so it is
never correct to see `'ok'` and still have `getWorkflow()` return non-null.

```ts
Sauron.startWorkflow('checkout');
// ... later
Sauron.endWorkflow(); // { status: 'ok', workflowId: '...' }
```

### `cancelWorkflow(name?, options?)`

```ts
function cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | current workflow | If given, must match the active workflow's name or the call is rejected. |
| `options.reason` | `string` | `'user'` | Free-form cancellation reason. Trimmed and capped at 120 characters. |

Cancels the active workflow: emits `$workflow_cancel` carrying `duration_ms`
and `reason`, then clears the active workflow. Same status values and
preconditions as `endWorkflow` (`'ok'` / `'not_active'` / `'name_mismatch'` /
`'disabled'`), including the rule that a malformed `name` reports
`'name_mismatch'`. `startWorkflow(..., { force: true })` uses this internally
with `reason: 'superseded'` when it replaces an active workflow.

```ts
Sauron.cancelWorkflow(); // reason defaults to 'user'
Sauron.cancelWorkflow('checkout', { reason: 'payment declined' });
```

### `getWorkflow()`

```ts
function getWorkflow(): ActiveWorkflow | null
```

Returns the active workflow — `{ workflowId, name, startedAt }` — or `null`
when none is active (including before `init()`, and after `close()`, which
resets it).

A workflow with no stamped activity for 30 minutes is surfaced as `abandoned`
when queried on the dashboard/API. That status is derived on read from the
last stamped event's timestamp — it is never stored, so there is nothing for
the client to do; an "abandoned" workflow that later receives another stamped
event simply reads as active again.

```ts
const active = Sauron.getWorkflow();
if (active) {
  console.log(`${active.name} running for`, Date.now() - Date.parse(active.startedAt), 'ms');
}
```

### `addBreadcrumb(breadcrumb, hint?)`

```ts
function addBreadcrumb(breadcrumb: BreadcrumbInput, hint?: Hint): void
```

| Field of `breadcrumb` | Type | Default | Description |
| --- | --- | --- | --- |
| `type` | `string` | `'default'` | Coarse kind, e.g. `'navigation'`. |
| `category` | `string` | `'default'` | Fine kind, e.g. `'ui.click'`, `'fetch'`. |
| `message` | `string \| null` | `null` | Short description. |
| `level` | `Level` | `'info'` | Severity. |
| `timestamp` | `string` | now, ISO-8601 UTC | Overrides the recorded time. |
| `data` | `Record<string, unknown> \| null` | `null` | Structured payload. |

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `hint` | `Hint` | `undefined` | Forwarded to `beforeBreadcrumb` only. |

The breadcrumb runs through `beforeBreadcrumb` and lands in the ring buffer.
Breadcrumbs are never sent on their own — the trail is copied onto every error
item. Returns `void`.

```ts
Sauron.addBreadcrumb({
  type: 'default',
  category: 'auth',
  level: 'info',
  message: 'token refreshed',
  data: { expires_in: 3600 },
});
```

### `setUser(user)`

```ts
function setUser(user: UserInput): void
```

| Field of `user` | Type | Default | Description |
| --- | --- | --- | --- |
| `id` | `string \| null` | `null` | User id; also becomes the `distinct_id` for later events. |
| `email` | `string \| null` | `null` | User email. |
| `traits` | `Record<string, unknown>` | `{}` | Arbitrary user traits. |

Pass `null` to clear the user entirely. The user is written to `context.user` on
every envelope, and onto `item.user` of error items while one is set. This is a
replace, not a merge. Returns `void`.

```ts
Sauron.setUser({ id: 'u_123', email: 'ada@example.com', traits: { plan: 'pro' } });
Sauron.setUser(null); // on logout
```

### `setTag(key, value)`

```ts
function setTag(key: string, value: string): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — (required) | Tag key. |
| `value` | `string` | — (required) | Tag value (strings only — tags are indexed). |

Sets one tag on the global scope; it is lifted onto every later error and event
item. Returns `void`.

```ts
Sauron.setTag('tenant', 'acme');
```

### `setTags(tags)`

```ts
function setTags(tags: Record<string, string>): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `tags` | `Record<string, string>` | — (required) | Batch of tags. |

Shallow-merges the batch into the scope, last-write-wins per key. Keys not
present are left alone. Returns `void`.

```ts
Sauron.setTags({ tenant: 'acme', tier: 'enterprise' });
```

### `setContext(name, block)`

```ts
function setContext(name: string, block: Record<string, unknown>): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — (required) | Block name, e.g. `'order'`. |
| `block` | `Record<string, unknown>` | — (required) | The block's contents. |

Replaces the whole named block (no deep merge). Dev-owned contexts are distinct
from the machine-detected `context` on the envelope and never overwrite it.
Returns `void`.

```ts
Sauron.setContext('order', { id: 7, total: 42.5 });
```

### `setExtra(key, value)`

```ts
function setExtra(key: string, value: unknown): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — (required) | Key in the freeform bag. |
| `value` | `unknown` | — (required) | Any JSON-serializable value. |

Sets one freeform value on the scope. Returns `void`.

```ts
Sauron.setExtra('feature_flags', ['new-cart', 'fast-checkout']);
```

### `flush(timeoutMs?)`

```ts
function flush(timeoutMs?: number): Promise<boolean>
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `timeoutMs` | `number` | `undefined` (wait indefinitely) | Give up after this many milliseconds. |

Drains the offline queue, then posts everything buffered in `maxBatch`-sized
envelopes. Resolves `true` on completion, `false` if `timeoutMs` elapsed first
or if `init()` was never called. Resolves `true` immediately when the client has
been disabled by a 401/403.

```ts
await Sauron.flush(2000);
```

### `close(timeoutMs?)`

```ts
function close(timeoutMs?: number): Promise<boolean>
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `timeoutMs` | `number` | `undefined` (wait indefinitely) | Passed straight to the inner flush. |

Flushes, then tears the SDK down: stops the flush timer and the `online`
listener, removes the unload listeners, clears the navigation hook and the
current screen, and restores every patched global in reverse order. Resolves to
the flush result. The client stays registered but disabled — call `init()` again
to restart.

```ts
await Sauron.close(2000);
```

### `getClient()`

```ts
function getClient(): SauronClient | null
```

Returns the active client, or `null` before `init()`.

```ts
const enabled = Sauron.getClient()?.isEnabled() ?? false;
```

### `SauronClient`

The client class, exported for typing and for the escape hatches below. You get
an instance from `init()` or `getClient()` — do not construct it yourself.

| Member | Signature | Description |
| --- | --- | --- |
| `options` | `readonly ResolvedOptions` | Fully-resolved options with defaults applied. |
| `dsn` | `readonly Dsn` | The parsed DSN. |
| `install()` | `(): void` | Install integrations + start the transport. Called by `init()`; a second call is a no-op. |
| `getScope()` | `(): Scope` | The mutable scope (user, breadcrumbs, tags, contexts, extra). |
| `isEnabled()` | `(): boolean` | `false` once the client has been explicitly `disable()`d/`teardown()`'d/`close()`d, **or** the transport has auto-disabled itself on a `401`/`403`. Computed from the transport's own state on every call, so a mid-session `401`/`403` flips this to `false` immediately — without the app ever calling `disable()`/`close()`. |
| `getDistinctId()` | `(): string \| null` | User id when identified, else the anonymous id (minting one if needed). |
| `getAnonymousId()` | `(): string \| null` | The anonymous id, or `null` if one was never needed. |
| `makeEnvelope(items)` | `(items: EnvelopeItem[]): Envelope` | Stamp a fresh envelope (new `sent_at`, current context) around `items`. |
| `addBreadcrumb(crumb, hint?)` | `(Breadcrumb, Hint?): void` | Full-shape breadcrumb, runs `beforeBreadcrumb`. |
| `captureItem(item, hint?)` | `(EnvelopeItem, Hint?): void` | Sampling + enrichment + workflow stamping + `beforeSend` + enqueue. |
| `flush(timeoutMs?)` | `(number?): Promise<boolean>` | Same as the module-level `flush`. |
| `disable()` | `(): void` | Stop accepting and sending; drops pending work. |
| `teardown()` | `(): void` | Restore globals and stop timers/listeners without flushing. |
| `close(timeoutMs?)` | `(number?): Promise<boolean>` | Flush, then `teardown()`. |

> **Workflow stamping happens inside `captureItem`.** That is why `track`,
> `captureException`, `captureMessage` and `trackTransaction` all pick up the active
> workflow automatically. If you hand-build an item and pass it to `captureItem` yourself,
> your own `workflow_id` / `workflow_name` win — the SDK will not overwrite them. Set
> **both or neither**: the server treats them as a pair and silently drops the attribution
> if only one is present, so the SDK logs a warning in that case. `identify` and
> breadcrumb-batch items are never stamped — the server has no workflow columns for them.

```ts
import { getClient } from '@edraj/sauron-browser';

const trail = getClient()?.getScope().getBreadcrumbs() ?? [];
```

### `parseDsn(dsn)` and `DsnError`

```ts
function parseDsn(dsn: string): Dsn
class DsnError extends Error
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `string` | — (required) | `https://<public_key>@<host>/<environment_id>`. |

Returns a `Dsn` with `raw`, `publicKey`, `host` (`host:port`), `hostname`,
`protocol` (`http` or `https`, no colon), `projectId` (the DSN's path
segment — despite the name, this is the **environment** id since the ingest
key now lives on the environment, not the app), `envelopeUrl`
(`<protocol>://<host>/api/<environment_id>/envelope`) and `beaconUrl` (the same
URL with `?k=<public_key>`).

Throws `DsnError` (message prefixed `[sauron] invalid DSN:`) for an empty or
non-string value, an unparseable URL, a protocol other than `http`/`https`, a
missing public key, a DSN that carries a password component, a missing host, or
a missing environment-id path segment.

```ts
import { parseDsn, DsnError } from '@edraj/sauron-browser';

try {
  const dsn = parseDsn('https://pk_test@ingest.example.com/42');
  console.log(dsn.envelopeUrl); // https://ingest.example.com/api/42/envelope
} catch (err) {
  if (err instanceof DsnError) console.error(err.message);
}
```

### `buildEnvelope(header, context, items)`

```ts
function buildEnvelope(
  header: EnvelopeHeader,
  context: Context,
  items: EnvelopeItem[],
): Envelope
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `header` | `EnvelopeHeader` | — (required) | `dsn`, `sdk`, `sent_at`, `release`. |
| `context` | `Context` | — (required) | `device`, `os`, `app`, `runtime`, `user`. |
| `items` | `EnvelopeItem[]` | — (required) | The payload items. |

A pure constructor for the canonical envelope shape (`header`, `context`,
`items`, in that order). Useful for tests and for hand-rolled delivery.

```ts
import { buildEnvelope, SDK_NAME, SDK_VERSION } from '@edraj/sauron-browser';

const envelope = buildEnvelope(
  {
    dsn: 'https://pk_test@localhost:8081/1',
    sdk: { name: SDK_NAME, version: SDK_VERSION },
    sent_at: new Date().toISOString(),
    release: null,
  },
  context,
  [item],
);
```

### `parseStackString(stack)`, `parseError(err)`, `isInAppFrame(filename)`

```ts
function parseStackString(stack: string | undefined | null): Frame[]
function parseError(err: unknown): Frame[]
function isInAppFrame(filename: string | null): boolean
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `stack` | `string \| undefined \| null` | — (required) | A raw `Error.stack` string. `null`/`undefined` yields `[]`. |
| `err` | `unknown` | — (required) | Any value; its string `.stack` is parsed, else `[]`. |
| `filename` | `string \| null` | — (required) | A frame filename. |

`parseStackString` handles both the V8/Chrome/Node/Edge (`at fn (file:line:col)`)
and the Firefox/Safari (`fn@file:line:col`) formats, skips non-frame lines,
caps at 50 frames keeping the ones nearest the crash, and returns them with the
**crashing frame last**. No symbolication happens client-side.

`isInAppFrame` returns `true` for bare/relative paths and same-origin absolute
URLs, and `false` for cross-origin URLs, `<anonymous>`, `node:*` and
`internal/*`.

```ts
import { parseError, isInAppFrame } from '@edraj/sauron-browser';

const frames = parseError(new Error('boom'));
const appFrames = frames.filter((f) => isInAppFrame(f.filename));
```

### `SDK_NAME`, `SDK_VERSION`

```ts
const SDK_NAME: string  // 'sauron.javascript'
const SDK_VERSION: string // '1.2.0'
```

The SDK identity embedded in `header.sdk` of every envelope.

### Exported types

All wire-contract and option types are exported for your own typing:

- Enums / unions: `Level`, `ItemType`, `TransactionOp`, `WorkflowStatus`.
- Item shapes: `Frame`, `Mechanism`, `ExceptionValue`, `Breadcrumb`, `ErrorItem`,
  `EventItem`, `IdentifyItem`, `BreadcrumbBatchItem`, `TransactionItem`,
  `EnvelopeItem`.
- Envelope shapes: `DeviceContext`, `OsContext`, `AppContext`, `RuntimeContext`,
  `UserContext`, `Context`, `SdkInfo`, `EnvelopeHeader`, `Envelope`.
- Input / option shapes: `Hint`, `UserInput`, `BeforeSend`, `BeforeBreadcrumb`,
  `TransportOptions`, `InitOptions`, `CaptureOptions`, `TrackOptions`,
  `ResolvedOptions`, `BreadcrumbInput`, `TransactionInput`, `Dsn`,
  `WorkflowResult`, `ActiveWorkflow`.

```ts
import type { EnvelopeItem, Hint, InitOptions } from '@edraj/sauron-browser';

const beforeSend = (item: EnvelopeItem, hint?: Hint): EnvelopeItem | null =>
  item.type === 'error' ? item : null;
const options: InitOptions = { dsn: '...', beforeSend };
```

## Automatic instrumentation

`init()` patches the following globals. Every patch chains or defers to the
original — the app's own handlers, console output and `fetch` results are never
swallowed — and every one is restored by `close()`.

On by default:

| Global | What it records |
| --- | --- |
| `window.onerror` | Error item, mechanism `{ type: 'onerror', handled: false }`, level `error`. The previous handler is still called with its original arguments. |
| `window.onunhandledrejection` | Error item from `event.reason`, mechanism `{ type: 'onunhandledrejection', handled: false }`, level `error`. |
| `console.log/info/warn/error/debug` | Breadcrumb, category `console`, level mapped (`warn`→`warning`, `error`→`error`, `debug`→`debug`, else `info`), message = arguments joined and truncated to 512 chars, `data: { arguments: n }`. Output is untouched. |
| `document` click listener (capture, passive) | Breadcrumb, category `ui.click`, message = a `tag#id.class` selector (up to 3 classes). Element text and attribute values are never serialized. |
| `history.pushState` / `replaceState` / `popstate` | Breadcrumb, type `navigation`, category `history`, `data: { from, to }` as paths. Same-path transitions are skipped. |
| `fetch` | Breadcrumb, category `fetch`, message `METHOD url`, `data: { method, url, status_code }`, level `warning` for status >= 400. |
| `XMLHttpRequest.prototype.open` / `send` | Breadcrumb, category `xhr`, same shape as `fetch`. |
| `document` `visibilitychange` + window `pagehide` | Beacon flush of the pending batch on unload. |
| window `online` | Drains the offline queue. |

Opt-in:

| Option | What it adds |
| --- | --- |
| `performance: true` | A `navigation` transaction for the initial page load (Navigation Timing, captured on `load`), an `http` transaction per instrumented `fetch` (`name` = `METHOD /path`, `status` `ok`/`error`), and a `navigation` transaction per SPA route change measured over one animation frame. No-op when `document` is undefined. |
| `screenTracking: true` | Sets the screen to the new path on each SPA History navigation, which emits a `$screen` event on change. |

Two guards keep the SDK from observing itself: a reentrancy flag held while SDK
code runs, and a denylist on the DSN host. Requests the transport makes are
therefore never turned into breadcrumbs or transactions. Wrappers are tagged, so
a double `init()` never stacks two layers on the same global.

Integrations that need an absent global (no `document`, no `history`, no
`XMLHttpRequest`, no writable `localStorage`) simply skip installation, so
importing and initializing during SSR does not throw.

## Scope & metadata

There is a single global scope per client, holding the user, the breadcrumb ring
buffer, `tags`, `contexts` and `extra`.

Precedence for `tags` / `contexts` / `extra`, lowest to highest:

1. **`init` defaults** — `tags`, `contexts`, `extra` are seeded into the scope
   when the client is constructed.
2. **Scope setters** — `setTag`, `setTags`, `setContext`, `setExtra` write into
   that same store, so they overwrite the init defaults for the keys they touch
   (last write wins) and leave the rest alone.
3. **Per-call overrides** — `hint.tags` / `hint.contexts` / `hint.extra` on
   `captureException` and `captureMessage`, and `options.tags` /
   `options.contexts` / `options.extra` on `track`. These win for that one item
   and never mutate the scope.

The merge is shallow, per top-level key: a per-call tag replaces the scope tag
of the same key; a per-call **context block replaces the whole same-named scope
block** (no deep merge); other blocks and keys are preserved. When the merged
result is empty the field is omitted from the wire item entirely — the backend
defaults it to `{}`.

Other scope data:

- **user** — `setUser()` replaces it wholesale; `identify()` also replaces it
  with `{ id, traits }`. It is written to `context.user` on every envelope, and
  additionally onto `item.user` of error items while a user is set.
- **breadcrumbs** — capped at `maxBreadcrumbs`, FIFO eviction. The whole trail
  is copied onto every error item; it is never sent on its own.
- **screen** — seeded by `init({ screen })`, changed by `setScreen()` (or
  `screenTracking`). Stamped on every event and error item.
  `TrackOptions.screen` overrides it for one event, `hint.screen` for one
  `captureException`.
- **identity** — `device_id` persists in `localStorage` under
  `sauron.device_id`; `session_id` persists in `sessionStorage` under
  `sauron.session_id`. Both fall back to a per-process in-memory id when Web
  Storage is unavailable.

```ts
Sauron.init({ dsn, tags: { tier: 'free' }, extra: { build: 'ci-42' } });
Sauron.setTag('tier', 'pro');                       // scope beats init default
Sauron.track('upgraded', {}, { tags: { tier: 'trial' } });
// -> event tags: { tier: 'trial' }, extra: { build: 'ci-42' }
```

## Bundlers & CDN

- `"type": "module"` with a dual build: `import` resolves `dist/index.js`
  (ESM), `require` resolves `dist/index.cjs`, types come from
  `dist/index.d.ts`. `package.json` itself is exported as `./package.json`;
  nothing else is deep-importable.
- `"sideEffects": false` — bundlers may drop unused exports. Importing the
  package does nothing on its own; instrumentation is installed by `init()`.
- Built with tsup, target `es2020`, with source maps and generated declarations.
- No UMD/IIFE build is shipped, so a plain `<script src="...">` global tag is
  not supported. On a CDN, use a module script against an ESM-serving CDN:

```html
<script type="module">
  import { Sauron } from 'https://esm.sh/@edraj/sauron-browser@1.2.0';
  Sauron.init({ dsn: 'https://pk_test@ingest.example.com/42' });
</script>
```

- The only runtime dependency is `fflate`, imported dynamically and only when
  the platform lacks `CompressionStream`. Bundlers will emit it as a separate
  async chunk.
- Initialize as early as possible — errors thrown before `init()` are not
  captured.

## Transport & delivery

**Batching.** Items are buffered in memory and flushed every `flushIntervalMs`
(default 5000 ms), immediately once `maxBatch` items are pending (default 30,
clamped to `[1, 1000]`), and on demand via `flush()`/`close()`. Each `flush()`
drains the offline queue first, then posts the buffered items in
`maxBatch`-sized envelopes.

**Request.** `POST <protocol>://<host>/api/<environment_id>/envelope` with:

```
Content-Type: application/json
X-Sauron-Key: <public_key>
Content-Encoding: gzip        # only when the body was compressed
```

The body is the canonical envelope — `header` + `context` + `items[]` —
identical across the JavaScript, Node, Python, Flutter and C# SDKs.

**Compression.** Payloads of 1024 bytes or more are gzipped with the native
`CompressionStream('gzip')` when available, falling back to a lazily imported
`fflate`. If neither works the envelope is sent uncompressed rather than
dropped. Smaller payloads are sent as plain JSON with no `Content-Encoding`.

**HTTP client.** The native `fetch` captured *before* the integrations wrap it,
so ingest traffic never instruments itself; `XMLHttpRequest` is the fallback
when `fetch` is absent. `keepalive: true` is set for bodies up to 64 KiB.

**Response handling.**

| Status | Action |
| --- | --- |
| `200`, `202` | Success — drop the batch. |
| `400` | Non-retryable — drop the batch. |
| `401`, `403` | Disable the client permanently: pending work is dropped and nothing further is sent until the next `init()`. |
| `408` | Retry with backoff. |
| `413` | Split the batch in half and retry each half. A single item that is still too large is parked in the offline queue. |
| `429` | Wait `Retry-After` (seconds or HTTP-date, clamped to 30 s; 1000 ms if unparseable), then retry. |
| `5xx` | Retry with backoff. |
| other `4xx` | Drop the batch. |
| network error / throw | Retry with backoff. |

Backoff is full-jitter: a uniform random delay in
`[0, min(30_000, 1000 * 2^attempt)]` ms. After 5 retries the serialized envelope
is parked in the offline queue.

**Offline queue.** A FIFO list under the `localStorage` key `sauron:queue:v1`,
byte-capped at `maxQueueBytes` (default 1 MiB); the oldest entries are evicted
first and at least one entry is always kept. It is drained at `init()`, at the
start of every `flush()`, and on the window `online` event. If a drained
envelope still fails, it is re-parked and draining stops to avoid a tight loop.
When `localStorage` is unavailable the queue is disabled and failed envelopes
are dropped.

**Page unload.** On `visibilitychange` → `hidden` and on `pagehide`, the pending
batch is chunked to 1000 items and handed to `navigator.sendBeacon` as an
uncompressed `application/json` Blob posted to
`POST /api/<environment_id>/envelope?k=<public_key>` (the key moves to the query
string because beacons cannot set headers). Chunks larger than 64 KiB, or a
`sendBeacon` that is unavailable or refuses, are parked in the offline queue for
the next page load.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing arrives, no client-side errors | The gateway is not exposed at `/api/{environment_id}/envelope` on the host root. A DSN cannot express a path prefix, so the SDK posts to the root path and a proxy that serves ingest under a sub-path silently 404s. | Expose ingest at `/api/{environment_id}/envelope` on the DSN host root. |
| Nothing arrives | `init()` was never called, or was called after the failing code ran. | Call `init()` first, as early in the page as possible. |
| `[sauron] client disabled` in the console, or `isEnabled()` unexpectedly `false` mid-session | The gateway returned 401/403 — wrong, revoked or foreign-project public key. `isEnabled()` flips to `false` automatically; nothing else changes. | Fix the DSN key/project; re-`init()` after correcting. |
| `DsnError` thrown at `init()` | Malformed DSN: bad protocol, missing public key, a password component, or a missing environment-id path segment. | Use `https://<public_key>@<host>/<environment_id>`. |
| Only some errors show up | `sampleRate` below 1 (errors and messages are sampled; events, identifies and transactions are not). | Set `sampleRate: 1`. |
| Errors arrive with no breadcrumbs | `maxBreadcrumbs: 0`, or `beforeBreadcrumb` returned `null`. | Raise `maxBreadcrumbs`; check the hook. |
| Items disappear silently | `beforeSend` returned `null`, or it threw (the original is then sent and a warning logged). | Enable `debug: true` and read the `[sauron]` logs. |
| Events lost when a tab closes after a busy session | The unload beacon chunk exceeded 64 KiB, or `sendBeacon` is unavailable. | Nothing to do — the payload is parked in `localStorage` and posted on the next page load. |
| Nothing persists in private mode | `localStorage`/`sessionStorage` are blocked, so the offline queue is disabled and ids fall back to in-memory. | Expected; reduce `flushIntervalMs` to shorten the loss window. |
| No transactions | `performance` defaults to `false`. | `init({ performance: true })`, or call `trackTransaction()` manually. |
| Screen is always `null` | Neither `init({ screen })`, `setScreen()` nor `screenTracking: true` was used. | Set one of them. |
| No debug output | `debug` defaults to `false`. | `init({ debug: true })`; logs are prefixed `[sauron]`. |

## Development

```bash
npm install
npm run typecheck    # tsc --noEmit
npm test             # vitest run
npm run test:watch   # vitest
npm run build        # tsup -> dist/ (esm + cjs + d.ts + sourcemaps)
npm run dev          # tsup --watch
```

`npm run prepublishOnly` chains typecheck, tests and build.

## License

AGPL-3.0-only — GNU Affero General Public License v3.0.

Repo: <https://github.com/edraj/sauron> — wiki:
<https://github.com/edraj/sauron/wiki>
