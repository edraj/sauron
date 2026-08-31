# @edraj/sauron-node

Server-side Node/TypeScript SDK for
[Sauron](https://github.com/edraj/sauron) — dispatch product-analytics events,
captured exceptions and performance transactions from your Node backends.

This is the **server-side** SDK. It has no browser, DOM or automatic
instrumentation: nothing is captured unless you call it (or explicitly opt in to
the process-level hooks). If you are instrumenting a web page, use
`@edraj/sauron-browser` (`sdks/js`) instead.

- Manual capture: `track`, `captureException`, `captureMessage`, `identify`,
  `trackTransaction`.
- Per-request isolation via `AsyncLocalStorage` — concurrent requests never leak
  user/tags/breadcrumbs into each other.
- Opt-in `uncaughtException` / `unhandledRejection` capture and
  `beforeExit`/`SIGTERM`/`SIGINT` graceful flush. Both default off.
- Buffered background transport: byte-bounded queue, optional FIFO disk
  persistence, gzip, exponential-backoff retry honoring `Retry-After`.
- Zero runtime dependencies — global `fetch`, `node:zlib`, `node:async_hooks`.

## Install

```bash
npm install @edraj/sauron-node
```

Requires **Node >= 18** (`engines.node`), for the global `fetch`. The package is
ESM-only (`"type": "module"`) and ships its own `.d.ts`.

## Quick start

```ts
import { init, track, captureException, close } from '@edraj/sauron-node';

init({
  dsn: 'https://<public_key>@<host>/<environment_id>',
  release: 'api@1.4.2',
});

track('order_completed', 'user-123', { total: 42.5, currency: 'USD' });

try {
  await chargeCard();
} catch (err) {
  captureException(err, { tags: { area: 'checkout' } });
}

// Buffered items go out on the 5s timer; flush explicitly before exit.
await close();
```

The SDK POSTs a canonical JSON envelope (`header` + `context` + `items[]`) to
`POST /api/{environment_id}/envelope` with an `X-Sauron-Key: <public_key>` header,
adding `Content-Encoding: gzip` once the body crosses the gzip threshold.

## Configuration

`init(options)` takes a single `InitOptions` object. Every field except `dsn` is
optional.

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `string` | — (**required**) | `https://<public_key>@<host>/<environment_id>`. A non-string throws `Error`; a malformed value throws `DsnError`. |
| `release` | `string \| null` | `null` | Written to `header.release`. |
| `tags` | `Record<string, string>` | `{}` | Default tags seeded into the global scope at init. |
| `contexts` | `Record<string, unknown>` | `{}` | Default named dev context blocks seeded into the global scope. Distinct from the machine `context` (device/os/app/runtime). |
| `extra` | `Record<string, unknown>` | `{}` | Default free-form extra values seeded into the global scope. |
| `sampleRate` | `number` | `1` | Error sample rate, clamped to `[0, 1]`. Applies to `captureException` **only** (see below). |
| `flushInterval` | `number` (ms) | `5000` | Background flush cadence. `<= 0` disables the timer entirely — you must call `flush()`/`close()` yourself. |
| `maxBatch` | `number` | `30` | Queue depth that triggers an eager flush. |
| `maxBreadcrumbs` | `number` | `100` | Breadcrumb ring-buffer size on the global scope. Clamped to `>= 0`; `0` drops all breadcrumbs. |
| `gzipThresholdBytes` | `number` | `1024` | Gzip the body once it is strictly larger than this. A negative value disables compression. |
| `maxQueueBytes` | `number` | `1048576` (1 MiB) | Drop-oldest byte cap for the in-memory send buffer. |
| `offlineDir` | `string` | `null` (off) | Directory for FIFO disk persistence of pending items (at-least-once across restarts). Created recursively if missing. |
| `maxRetries` | `number` | `3` | Retries **after** the first attempt for transient failures. Clamped to `>= 0`. |
| `autoCaptureUnhandled` | `boolean` | `false` | Install `uncaughtException` / `unhandledRejection` handlers. |
| `autoShutdown` | `boolean` | `false` | Install `beforeExit` / `SIGTERM` / `SIGINT` handlers that `close()`. |
| `beforeSend` | `BeforeSend` | `undefined` | `(item, hint?) => item \| null`. Runs on every outgoing item just before enqueue; `null` drops it. |
| `beforeBreadcrumb` | `BeforeBreadcrumb` | `undefined` | `(crumb, hint?) => crumb \| null`. Runs on every breadcrumb before it is stored; `null` drops it. |
| `fetchImpl` | `FetchLike` | global `fetch` | Injected HTTP sender (tests). Construction throws if neither this nor a global `fetch` is available. |
| `debug` | `boolean` | `false` | Log transport decisions to `console.warn` with a `[sauron]` prefix. |

Notes that are easy to get wrong:

- `sampleRate` is checked **only** in `captureException`. `captureMessage`,
  `track`, `identify` and `trackTransaction` are never sampled.
- `init` seeds `tags`/`contexts`/`extra` into the process-wide **global scope**;
  it never clears it. Calling `init` twice accumulates rather than replaces.
- The per-envelope item cap is a fixed `1000` (matching the server limit) and is
  not exposed through `InitOptions`; large backlogs are split across envelopes.

Fully-populated example:

```ts
import { init, type EnvelopeItem, type Breadcrumb } from '@edraj/sauron-node';

const client = init({
  dsn: 'https://pk_live_abc@ingest.example.com/42',
  release: 'api@1.4.2',
  tags: { service: 'checkout-api', region: 'eu-west-1' },
  contexts: { deployment: { cluster: 'eu-1', pod: process.env.HOSTNAME } },
  extra: { build_sha: process.env.GIT_SHA },
  sampleRate: 0.5,
  flushInterval: 2000,
  maxBatch: 50,
  maxBreadcrumbs: 50,
  gzipThresholdBytes: 2048,
  maxQueueBytes: 4 * 1024 * 1024,
  offlineDir: '/var/lib/sauron/pending',
  maxRetries: 5,
  autoCaptureUnhandled: true,
  autoShutdown: false,
  beforeSend: (item: EnvelopeItem) => {
    if (item.type === 'event' && 'email' in item.properties) {
      item.properties.email = '[redacted]';
    }
    return item;
  },
  beforeBreadcrumb: (crumb: Breadcrumb) =>
    crumb.category === 'secret' ? null : crumb,
  debug: true,
});
```

## API reference

Every module-level capture function delegates to the client created by the most
recent `init` and is a **no-op before `init`** (and after `close`). They never
throw and never return a value.

### `init(options)`

```ts
function init(options: InitOptions): SauronClient
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `options` | `InitOptions` | — (required) | See [Configuration](#configuration). |

Returns the `SauronClient` it created, and installs it as the active client.
Throws `Error` when `options.dsn` is not a string, and `DsnError` when the DSN
is malformed.

```ts
const client = init({ dsn: 'https://pk@ingest.example.com/42' });
```

### `getClient()`

```ts
function getClient(): SauronClient | null
```

Returns the client created by the most recent `init`, or `null` before init /
after `close()`.

```ts
if (getClient() === null) console.warn('sauron not initialized');
```

### `track(event, distinctId, properties?, options?)`

```ts
function track(
  event: string,
  distinctId: string,
  properties?: Record<string, unknown>,
  options?: MetadataOptions,
): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `event` | `string` | — (required) | Event name. A non-string or empty string drops the call silently. |
| `distinctId` | `string` | — (required) | User/actor identity. A non-string or empty string drops the call silently. |
| `properties` | `Record<string, unknown>` | `{}` | Event properties. |
| `options` | `MetadataOptions` | `{}` | `{ tags?, contexts?, extra? }` merged over the active scope's metadata. |

Emits an `event` item. Returns `void`.

```ts
track('order_completed', 'user-123', { total: 42.5 }, {
  tags: { plan: 'pro' },
  extra: { trace_id: req.id },
});
```

### `captureException(error, options?)`

```ts
function captureException(error: unknown, options?: CaptureExceptionOptions): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `error` | `unknown` | — (required) | An `Error`, a string, or any object with `name`/`message`. Anything else is stringified. |
| `options.tags` | `Record<string, string>` | `{}` | Merged **over** the scope's tags. |
| `options.contexts` | `Record<string, unknown>` | `{}` | Merged by block name over the scope's contexts. |
| `options.extra` | `Record<string, unknown>` | `{}` | Merged by key over the scope's extra. |
| `options.user` | `Partial<ErrorUser> \| null` | `null` | `{ id?, email?, username? }`; missing fields become `null`. When omitted, the scope's user is used. |
| `options.level` | `Level` | `'error'` | `'debug' \| 'info' \| 'warning' \| 'error' \| 'fatal'`. |
| `options.handled` | `boolean` | `true` | Sets `exception.mechanism.handled`. |
| `options.fingerprint` | `string[] \| null` | `null` | Grouping override, honored verbatim by the backend. |

Emits an `error` item carrying a parsed stack trace (crash frame **last**, max 50
frames) plus the active scope's breadcrumb trail. Subject to `sampleRate`.
Returns `void`.

```ts
try {
  await settleInvoice(id);
} catch (err) {
  captureException(err, {
    level: 'fatal',
    handled: false,
    user: { id: 'user-123', email: 'a@b.co' },
    tags: { area: 'billing' },
    fingerprint: ['invoice-settle-failure'],
  });
}
```

### `captureMessage(message, level?, options?)`

```ts
function captureMessage(message: string, level?: Level, options?: MetadataOptions): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `message` | `string` | — (required) | Message body. |
| `level` | `Level` | `'info'` | Severity. |
| `options` | `MetadataOptions` | `{}` | `{ tags?, contexts?, extra? }` merged over the active scope. |

Emits an `error` item with `exception.type = 'Message'`, an empty stack trace and
`message` set. Not sampled. Returns `void`.

```ts
captureMessage('cache warm-up finished', 'info', { tags: { job: 'warmup' } });
```

### `identify(distinctId, traits?)`

```ts
function identify(distinctId: string, traits?: Record<string, unknown>): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `distinctId` | `string` | — (required) | Identity to attach traits to. Empty/non-string drops the call. |
| `traits` | `Record<string, unknown>` | `{}` | Trait map. |

Emits an `identify` item with `anonymous_id: null`. Scope metadata is **not**
attached to identify items. Returns `void`.

```ts
identify('user-123', { plan: 'pro', seats: 12 });
```

### `trackTransaction(input)`

```ts
function trackTransaction(input: TransactionInput): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `input.name` | `string` | — (required) | Transaction name. Empty/non-string drops the call. |
| `input.op` | `string` | `'custom'` | Operation class: `navigation`, `http`, `resource`, `screen_load`, `custom`. |
| `input.duration_ms` | `number` | — (required) | Wall-clock duration in ms (fractional allowed). |
| `input.status` | `string` | omitted | Free-form outcome, e.g. `'ok'`. |
| `input.http_method` | `string` | omitted | e.g. `'GET'`. |
| `input.http_status` | `number` | omitted | e.g. `200`. |
| `input.url` | `string` | omitted | Request URL/path. |
| `input.distinct_id` | `string` | scope user's `id`, else omitted | Explicit value wins over the scope. |
| `input.tags` | `Record<string, string>` | omitted | Indexed string→string labels. Filter with `@tag.key:value` on the Transactions page. |
| `input.extra` | `Record<string, unknown>` | omitted | Freeform JSON — request body, response body, SQL text, row counts. Searchable with `extra.key:value`. |

Emits a `transaction` item. Absent optional fields are omitted from the wire JSON
rather than serialized as `null`. Returns `void`.

**`tags` and `extra` are per-call only.** Unlike `track()` and
`captureException()`, a transaction does **not** inherit the scope:
`setTag()` / `setExtra()` defaults are not merged in. Transactions are the
highest-volume signal a service emits — one per request and per query — so
inheriting a global blob would write it onto every row.

`extra` is serialized and capped at **16 KB**
(`MAX_TRANSACTION_EXTRA_BYTES`, exported from `transaction-extra`). Past that
the whole map is replaced with `{ _truncated: true, _bytes: N }` and the
dashboard says so on the row. The cap is not cosmetic: envelopes are batched,
and one oversized body would push the whole envelope past the ingest limit and
drop every unrelated span sent with it.

Nothing in `extra` is scrubbed. Use `beforeSend` for redaction, and think twice
before attaching a body that can carry tokens, passwords or personal data.

```ts
const started = Date.now();
await handler(req, res);
trackTransaction({
  name: 'GET /api/users',
  op: 'http',
  duration_ms: Date.now() - started,
  status: 'ok',
  http_method: 'GET',
  http_status: 200,
  url: '/api/users',
});
```

#### Example: an Express route, with request and response bodies

`res.json` is wrapped rather than read afterwards, because by the time the
`finish` event fires the payload is already on the wire and gone.

```ts
import express from 'express';
import { trackTransaction } from '@edraj/sauron-node';

export function tracedRoute(routeLabel: string, handler: express.RequestHandler) {
  return async (req: express.Request, res: express.Response, next: express.NextFunction) => {
    const started = Date.now();
    let responseBody: unknown;

    // Capture on the way out. Reading it in `finish` is too late — the body
    // has already been serialized and released by then.
    const json = res.json.bind(res);
    res.json = (body: unknown) => {
      responseBody = body;
      return json(body);
    };

    res.on('finish', () => {
      trackTransaction({
        name: routeLabel,              // 'POST /orders', NOT '/orders/8412'
        op: 'http',
        duration_ms: Date.now() - started,
        http_method: req.method,
        http_status: res.statusCode,
        url: req.originalUrl,
        status: res.statusCode < 400 ? 'ok' : 'error',
        distinct_id: (req as { userId?: string }).userId,
        tags: { route: routeLabel, tier: (req as { plan?: string }).plan ?? 'free' },
        extra: {
          request: req.body,
          response: responseBody,
          query: req.query,
          // Header VALUES are omitted on purpose — `authorization` and
          // `cookie` live there.
          request_headers: Object.keys(req.headers),
        },
      });
    });

    try {
      await handler(req, res, next);
    } catch (err) {
      next(err);
    }
  };
}

const app = express();
app.post('/orders', tracedRoute('POST /orders', createOrder));
```

On the dashboard: **Transactions → the row → expand**. Both bodies render as a
JSON tree, and every one of these finds it:

```text
extra.response:~9001        # substring, inside the stored response body
@tag.route:"POST /orders"   # indexed tag
op:http http.status:>=500   # the failures
duration:>2s                # the slow ones
```

#### Example: a SQL query (`pg`)

Put the **statement** in `extra` and keep `name` a stable label — a query with
literals baked in would mint a new dashboard row per execution.

```ts
import { Pool } from 'pg';
import { trackTransaction } from '@edraj/sauron-node';

const pool = new Pool();

export async function tracedQuery<T>(label: string, sql: string, params: unknown[] = []) {
  const started = Date.now();
  try {
    const result = await pool.query<T>(sql, params);
    trackTransaction({
      // The LABEL, not the statement. `op` is free-form on this SDK, so a
      // 'db' op is stored as-is — but note the browser SDK coerces anything
      // outside navigation|http|resource|screen_load|custom to 'custom', so
      // use a tag if you need the two to agree.
      name: label,
      op: 'db',
      duration_ms: Date.now() - started,
      status: 'ok',
      tags: { db: 'postgres', table: 'orders' },
      extra: {
        statement: sql,
        row_count: result.rowCount,
        // Bind PARAMETERS are user data. Log them only if you have decided
        // that is acceptable, or log their shape instead.
        params,
      },
    });
    return result;
  } catch (err) {
    trackTransaction({
      name: label,
      op: 'db',
      duration_ms: Date.now() - started,
      status: 'error',
      tags: { db: 'postgres', table: 'orders' },
      extra: { statement: sql, error: String(err) },
    });
    throw err;
  }
}

await tracedQuery(
  'SELECT orders',
  'SELECT id, total FROM orders WHERE user_id = $1 ORDER BY created_at DESC LIMIT 20',
  [userId],
);
```

Then `@tag.table:orders duration:>500ms` is your slow-query list.

### `addBreadcrumb(crumb)`

```ts
function addBreadcrumb(crumb: BreadcrumbInput): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `crumb.type` | `string` | `'default'` | Crumb kind, e.g. `'http'`, `'navigation'`. |
| `crumb.category` | `string` | `null` | Free-form category. |
| `crumb.message` | `string` | `null` | Human-readable message. |
| `crumb.level` | `Level` | `null` | Severity. |
| `crumb.data` | `Record<string, unknown>` | `{}` | Structured payload. |

The crumb is stamped with an ISO-8601 `timestamp`, passed through
`beforeBreadcrumb` (a `null` return drops it), then pushed onto the **active**
scope's ring buffer. Once the buffer exceeds `maxBreadcrumbs`, the oldest crumbs
are evicted. Breadcrumbs are attached to error items only. Returns `void`.

```ts
addBreadcrumb({
  type: 'http',
  category: 'outbound',
  message: 'POST https://psp.example.com/charge',
  level: 'info',
  data: { status: 502, attempt: 2 },
});
```

### `startWorkflow(name, options?)`, `endWorkflow(name?)`, `cancelWorkflow(name?, options?)`, `getWorkflow()`

```ts
function startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult
function endWorkflow(name?: string): WorkflowResult
function cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult
function getWorkflow(): ActiveWorkflow | null
```

Workflows are an entirely **optional** way to bound a named span of activity
(e.g. `"checkout"`, `"password_reset"`) and have every event/error/transaction
captured while it is active stamped with it, so the dashboard can group them.
An app that never calls any of these four functions emits byte-identical
telemetry to one that predates this feature — no field is added to any item.

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` (start) | `string` | — (required) | Workflow name. Trimmed; rejected if empty after trimming or over 120 chars — **rejected, never truncated**. |
| `options.force` (start) | `boolean` | `false` | If `true` and a workflow is already active, it is superseded (see below) instead of blocking the new one. |
| `name` (end/cancel) | `string` | active workflow's name | If given, must match the active workflow's (trimmed) name or the call is a no-op. |
| `options.reason` (cancel) | `string` | `'user'` | Trimmed and capped at 120 chars. Never sent on `endWorkflow`. |

`WorkflowResult` is `{ status: WorkflowStatus; workflowId?: string }` —
`workflowId` is present only when `status` is `'ok'`. `WorkflowStatus` is
exactly six values, never a seventh:

| Status | Meaning |
| --- | --- |
| `ok` | The call took effect. |
| `already_active` | `startWorkflow` while one is already active and `force` was not set. The existing workflow is untouched. |
| `not_active` | `endWorkflow`/`cancelWorkflow` with none active. |
| `name_mismatch` | `endWorkflow`/`cancelWorkflow` with an explicit `name` that does not match the active workflow's — **including** a `name` that is itself malformed (empty after trim, or over 120 chars). A bad name on end/cancel is a mismatch against what's actually active, not an `invalid_name` (that status is only reachable from `startWorkflow`). |
| `invalid_name` | `startWorkflow` with an empty (after trim) or over-120-char name. |
| `disabled` | Nothing happened: before `init()`/after `close()`, after the transport has auto-disabled itself (401/403), **or an unexpected internal error**. Telemetry never throws into your code, so an internal failure is reported this way rather than propagating; if it happens after `startWorkflow` had already set the workflow locally, the workflow is still live and this case is instead reported as `ok` (see below). |

`startWorkflow` mints a fresh **client-generated UUID v4** (`workflowId`, via
`node:crypto`'s `randomUUID`) — never a session id, a hash of the name, or
anything else deterministic. The server's rollup key is
`(app_id, workflow_id)` **app-wide**, so a reused or derived id would merge
unrelated requests'/environments' counts into one row.

`startWorkflow(name, { force: true })` while a workflow is already active
first emits `$workflow_cancel` for it with `reason: 'superseded'`, then starts
the new one — both as a single call. Without `force`, an active workflow makes
the call a no-op returning `already_active`.

`endWorkflow`/`cancelWorkflow` emit `$workflow_end`/`$workflow_cancel`
(respectively) carrying `duration_ms`, then clear the workflow. A workflow is
also considered **abandoned** if the server sees no further stamped item for
it within 30 minutes — this is derived purely on read, server-side, from the
last item's timestamp; there is nothing to configure and no client-side timer
or action required. If an event stamped with that workflow arrives later
anyway (a slow retry, an offline-persisted item flushed after a restart, …),
it simply reads as active again — "abandoned" is not a terminal state you can
race.

`getWorkflow()` returns the workflow active on the **current scope** (see
below), or `null` if none — it takes no client and works the same regardless
of whether the SDK is initialized, exactly like `getCurrentScope()`.

**Attribution and `unique_users`.** The three lifecycle events are attributed
to the active scope's user id, and to an **empty** `distinct_id` when no user
has been identified (a background job, a pre-auth request). This is
deliberate, and it is why they bypass the empty-`distinctId` guard that drops
an ordinary `track()` call: the ingest pipeline stores an empty `distinct_id`
as SQL `NULL` on the workflow row, and the dashboard's per-workflow
`unique_users` figure is a `COUNT(DISTINCT distinct_id)`, which skips `NULL`s.
So anonymous runs contribute *nothing* to that count rather than collapsing
into one fake bucket — an anonymous-heavy workflow like `guest_checkout`
reports honest zeros instead of a misleading `1`. Call `setUser({ id })` (or
set a scoped user) before `startWorkflow` if you want those runs counted.

```ts
app.post('/checkout', async (req, res) => {
  const started = startWorkflow('checkout');
  addBreadcrumb({ type: 'http', message: 'checkout started' });
  track('checkout_started', req.userId ?? 'anon');
  try {
    await charge(req.body);
    track('checkout_completed', req.userId ?? 'anon');
    endWorkflow('checkout');
  } catch (err) {
    captureException(err, { tags: { area: 'checkout' } });
    cancelWorkflow('checkout', { reason: 'payment_declined' });
    throw err;
  }
  res.sendStatus(200);
});
```

**Request-scoped via `AsyncLocalStorage`, not a module global.** Unlike the
browser SDK, this server SDK never holds the active workflow in a
module-level variable — that would leak one HTTP request's workflow into
every other concurrent request's telemetry. Instead it lives on the current
`Scope` (the same `AsyncLocalStorage`-backed mechanism `user`/`tags`/
`breadcrumbs` already use — see [Scope & metadata](#scope--metadata)):
`startWorkflow` inside a `withScope`/`runWithAsyncScope` block sets it on that
request's isolated child scope, and it is gone the moment the block returns,
never visible to any other concurrent request.

This means workflow state follows the **same async call chain** rules as the
rest of the scope: it propagates automatically across `await`s and work
scheduled from within the scoped callback (timers, promise continuations),
but code that runs **outside** that chain — a listener registered on a
long-lived `EventEmitter` before you ever called `withScope`, a job resumed
from a persisted queue/message broker, or anything invoked from a fresh async
root — will not see it. `getCurrentScope()` (and therefore `getWorkflow()`) in
that detached code falls back to the global scope, not the request's. If work
must cross into a detached async context, capture `getWorkflow()`'s
`workflowId`/`name` explicitly before you leave the scope and pass them along,
rather than relying on ambient state to still be there.

### `setUser(user)`

```ts
function setUser(user: User | null): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `user` | `User \| null` | — (required) | `{ id?, email?, username? }` (all `string \| null`), or `null` to clear. |

Sets the user on the **active** scope — the `withScope` child if you are inside
one, otherwise the process-wide global scope. Used to fill an error item's `user`
when the capture call did not supply one, and as the `distinct_id` fallback for
`trackTransaction`. Returns `void`.

```ts
setUser({ id: 'user-123', email: 'a@b.co' });
setUser(null); // clear
```

### `setTag(key, value)`

```ts
function setTag(key: string, value: string): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — (required) | Tag key. |
| `value` | `string` | — (required) | Tag value (tags are string→string). |

Sets one tag on the active scope, overwriting any existing value for `key`.

```ts
setTag('route', '/api/orders/:id');
```

### `setTags(tags)`

```ts
function setTags(tags: Record<string, string>): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `tags` | `Record<string, string>` | — (required) | Tags to merge. |

Shallow-merges `tags` into the active scope's tags (`Object.assign`); existing
keys not present in `tags` survive.

```ts
setTags({ route: '/api/orders', tenant: 'acme', canary: 'true' });
```

### `setContext(key, context)`

```ts
function setContext(key: string, context: Record<string, unknown> | unknown): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — (required) | Block name. |
| `context` | `Record<string, unknown> \| unknown` | — (required) | Block value; stored verbatim. |

Sets a **named block** on the active scope. Blocks are replaced wholesale by
name, never deep-merged. This is the developer-assignable `contexts` map — it is
separate from the machine `context` (device/os/app/runtime) the SDK builds once
at init.

```ts
setContext('order', { id: 'ord_9', items: 3, currency: 'USD' });
```

### `setExtra(key, value)`

```ts
function setExtra(key: string, value: unknown): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — (required) | Extra key. |
| `value` | `unknown` | — (required) | Any JSON-serializable value. |

Sets one free-form value on the active scope's `extra` map.

```ts
setExtra('trace_id', req.headers['x-trace-id']);
```

### `withScope(cb)`

```ts
function withScope<T>(cb: (scope: Scope) => T): T
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `cb` | `(scope: Scope) => T` | — (required) | Runs with an isolated child scope, which is also passed as the argument. |

Clones the current scope, runs `cb` inside an `AsyncLocalStorage` context bound to
that clone, and returns whatever `cb` returns (so an `async` callback yields a
promise you can await). For the duration of `cb` — **including across `await`s
and any async work started inside it** — `getCurrentScope()` returns the child.
Mutations to the child never propagate back to the parent/global scope.

```ts
await withScope(async (scope) => {
  scope.setUser({ id: 'user-123' });
  scope.setTag('job', 'nightly-invoice');
  await runJob(); // captures inside see the child scope
}); // child discarded here
```

### `runWithAsyncScope(cb)`

```ts
function runWithAsyncScope<T>(cb: () => T): T
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `cb` | `() => T` | — (required) | Same as `withScope`, but the callback takes no argument. |

Identical semantics to `withScope`; use it when you prefer the module-level
`setUser`/`setTag` helpers over the `Scope` handle.

```ts
await runWithAsyncScope(async () => {
  setUser({ id: 'user-123' });
  setTag('job', 'nightly-invoice');
  await runJob();
});
```

### `configureScope(cb)`

```ts
function configureScope(cb: (scope: Scope) => void): void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `cb` | `(scope: Scope) => void` | — (required) | Receives the **currently active** scope. |

Mutates the active scope in place — it does **not** create a child and does not
isolate anything. Outside a `withScope`, that is the process-wide global scope
and the mutation is permanent for the process. Returns `void` (the callback's
return value is discarded).

```ts
// withScope: temporary + isolated
withScope((scope) => {
  scope.setTag('request_id', 'r-1');   // gone after the callback
});

// configureScope: mutates whatever is active right now
configureScope((scope) => {
  scope.setTag('service', 'checkout'); // process-wide, permanent
});

// inside a withScope, configureScope targets the child
withScope(() => {
  configureScope((scope) => scope.setTag('request_id', 'r-2')); // child only
});
```

### `getCurrentScope()` / `getGlobalScope()`

```ts
function getCurrentScope(): Scope
function getGlobalScope(): Scope
```

`getCurrentScope()` returns the async-local child inside a `withScope` /
`runWithAsyncScope` block, else the global scope. `getGlobalScope()` always
returns the process-wide scope, even from inside a child.

```ts
withScope((child) => {
  getCurrentScope() === child;          // true
  getGlobalScope().setTag('boot', 'ok'); // reaches past the child
});
```

### `flush()`

```ts
function flush(): Promise<void>
```

Zero-argument. Drains the queue and POSTs it now, in bounded chunks; resolves
once the in-flight sends have settled (there is **no** timeout parameter — a slow
ingest with retries can keep the promise pending for up to
`maxRetries` backoffs, each capped at 30 s). Resolves immediately when the SDK is
not initialized, and never rejects — transport failures are swallowed and the
batch is re-buffered.

```ts
await flush();
```

### `close()`

```ts
function close(): Promise<void>
```

Zero-argument. Clears the active client, then flushes it, stops the background
timer and uninstalls any process hooks installed by `autoCaptureUnhandled` /
`autoShutdown`. Resolves immediately if the SDK is not initialized. After
`close()` every module-level capture function is a no-op again until the next
`init`.

```ts
await close();
```

### `installAutoCapture(client, options?)`

```ts
function installAutoCapture(
  client: SauronClient,
  options?: AutoCaptureOptions,
): () => void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `client` | `SauronClient` | — (required) | Target client. |
| `options.process` | `ProcessLike` | Node's `process` | Injected process object (tests). |

Registers two listeners and returns an **uninstaller**. Idempotent per client —
installing twice returns the first uninstaller and registers nothing new.

- `uncaughtException` → `captureException(err, { level: 'fatal', handled: false })`,
  then `flush()`, then — only if this SDK is the *sole* `uncaughtException`
  listener — `process.exit(1)`, preserving Node's default crash behavior. If any
  other listener is registered, that listener owns the process's fate.
- `unhandledRejection` → `captureException(reason, { level: 'error', handled: false })`
  then `flush()`. Never exits on its own; Node's own `unhandledRejection` mode
  still governs.

Re-entrancy is guarded, so a throw inside the capture path cannot loop. Prefer
`init({ autoCaptureUnhandled: true })`, which calls this for you and tears it
down on `close()`.

```ts
const client = init({ dsn: DSN });
const uninstall = installAutoCapture(client);
// later
uninstall();
```

### `installShutdownHooks(client, options?)`

```ts
function installShutdownHooks(
  client: SauronClient,
  options?: AutoCaptureOptions,
): () => void
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `client` | `SauronClient` | — (required) | Target client. |
| `options.process` | `ProcessLike` | Node's `process` | Injected process object (tests). |

Registers three listeners and returns an uninstaller. Idempotent per client. All
three are guarded by a `closing` latch, so only the first one to fire runs.

- `beforeExit` → `client.close()`. Does **not** force an exit.
- `SIGTERM` → `client.close()` then `process.exit(143)`.
- `SIGINT` → `client.close()` then `process.exit(130)`.

Prefer `init({ autoShutdown: true })`. Note the exit is unconditional once the
signal fires — if you need to drain HTTP connections first, leave `autoShutdown`
off and call `close()` yourself from your own handler.

```ts
const client = init({ dsn: DSN });
const uninstall = installShutdownHooks(client);
```

### `class SauronClient`

Returned by `init`, or constructible directly for a second, independent client
(`new SauronClient(options)` with the same `InitOptions`). Its instance methods
mirror the module-level functions exactly, minus the "no-op before init"
behavior: `track`, `captureException`, `captureMessage`, `identify`,
`trackTransaction`, `addBreadcrumb`, `startWorkflow`, `endWorkflow`,
`cancelWorkflow`, `flush()`, `close()`, plus `isEnabled()` (`false` once the
transport has auto-disabled itself on a 401/403 — the same check `disabled`
statuses are gated on). `getWorkflow()` is **not** an instance method: it
reads the current scope directly and is client-agnostic, like
`getCurrentScope()`.

```ts
import { SauronClient } from '@edraj/sauron-node';

const audit = new SauronClient({ dsn: AUDIT_DSN, flushInterval: 1000 });
audit.track('audit_written', 'system', { table: 'ledger' });
await audit.close();
```

Note that all clients share the same process-wide global scope and the same
`AsyncLocalStorage`, so scope state is not per-client.

### `class Scope`

The object handed to `withScope` / `configureScope`. All mutators return `this`
for chaining.

| Member | Signature | Description |
| --- | --- | --- |
| `data` | `ScopeData` | `{ user, tags, contexts, extra, breadcrumbs }` — readable directly. |
| `setUser` | `(user: User \| null) => this` | Set/clear the user. |
| `setTag` | `(key: string, value: string) => this` | Set one tag. |
| `setTags` | `(tags: Record<string, string>) => this` | Shallow-merge tags. |
| `setContext` | `(key: string, context: unknown) => this` | Set a named block. |
| `setExtra` | `(key: string, value: unknown) => this` | Set one extra value. |
| `addBreadcrumb` | `(crumb: BreadcrumbInput \| Breadcrumb) => this` | Push a crumb, evicting the oldest past the cap. Bypasses `beforeBreadcrumb` — use the module-level `addBreadcrumb` if you want that hook. |
| `setMaxBreadcrumbs` | `(max: number) => void` | Resize the ring buffer (clamped `>= 0`), trimming immediately. |
| `clone` | `() => Scope` | Snapshot with no shared mutable containers. |
| `applyToErrorItem` | `(item) => void` | Layer this scope onto an error item (scope *under* per-call). |
| `mergeMetadata` | `(overrides?) => { tags?, contexts?, extra? }` | Merge for non-error items, omitting empty maps. |

```ts
withScope((scope) => {
  scope.setUser({ id: 'u1' }).setTag('tier', 'gold').setExtra('shard', 3);
});
```

### `parseDsn(dsn)` and `DsnError`

```ts
function parseDsn(dsn: string): Dsn
class DsnError extends Error
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `string` | — (required) | `https://<public_key>@<host>/<environment_id>`. |

Returns a `Dsn`: `{ raw, publicKey, host, hostname, protocol, projectId,
envelopeUrl }` — `projectId` is the DSN's path segment (despite the name, this
is the **environment** id since the ingest key now lives on the environment,
not the app) — where `envelopeUrl` is
`{protocol}://{host}/api/{environment_id}/envelope`. Throws `DsnError` (`name`
is `'DsnError'`, message prefixed `[sauron] invalid DSN:`) for an
empty/unparseable string, a protocol other than `http`/`https`, a missing
public key, a **present password** (a DSN must never carry a secret), a
missing host, or a missing environment-id path segment.

```ts
import { parseDsn, DsnError } from '@edraj/sauron-node';

try {
  const dsn = parseDsn(process.env.SAURON_DSN!);
  console.log(dsn.envelopeUrl); // https://ingest.example.com/api/42/envelope
} catch (err) {
  if (err instanceof DsnError) process.exit(1);
}
```

### `parseError(err)`, `parseStackString(stack)`, `isInAppFrame(filename)`

```ts
function parseError(err: unknown): Frame[]
function parseStackString(stack: string | undefined | null): Frame[]
function isInAppFrame(filename: string | null): boolean
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `err` | `unknown` | — (required) | Any value; a string `.stack` property is parsed, anything else yields `[]`. |
| `stack` | `string \| undefined \| null` | — (required) | A raw V8 `Error.stack` string. |
| `filename` | `string \| null` | — (required) | A frame filename. |

`parseStackString` returns normalized `Frame`s with the **crashing frame last**
(raw V8 stacks are crash-first, so the list is reversed), capped at 50 frames
nearest the crash, with `file://` prefixes stripped and no symbolication.
`isInAppFrame` returns `false` for `null`, `<anonymous>`, `node:*`, `internal/*`,
`node internal*` and anything containing `node_modules`; `true` otherwise.

```ts
const frames = parseError(new Error('boom'));
frames.at(-1); // the crash site
isInAppFrame('/srv/app/routes/orders.js'); // true
isInAppFrame('node:internal/process/task_queues'); // false
```

### `describeError(error)`

```ts
function describeError(error: unknown): { type: string; value: string | null }
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `error` | `unknown` | — (required) | Any thrown value. |

Derives the `exception.type` / `exception.value` pair the SDK puts on the wire:
an `Error` yields `{ type: err.name || 'Error', value: err.message || null }`; a
string yields `{ type: 'Error', value: theString }`; an object yields its
`name`/`message` when they are strings; `undefined` yields a `null` value and
anything else is `String()`-ified.

```ts
describeError(new TypeError('x is not a function'));
// { type: 'TypeError', value: 'x is not a function' }
describeError('plain failure'); // { type: 'Error', value: 'plain failure' }
```

### `class Transport`

The buffered background sender, exported for advanced/embedding use. `init`
constructs one for you from `InitOptions`; you rarely need it directly. Its
`TransportConfig` accepts three knobs `InitOptions` does not expose:
`maxItemsPerEnvelope` (default `1000`), `retryBaseMs` (default `200`) and the
`sleep`/`random` test seams. Public methods: `enqueue(item)`, `flush()`,
`close()`.

```ts
import { Transport, parseDsn } from '@edraj/sauron-node';

const transport = new Transport({
  dsn: parseDsn(process.env.SAURON_DSN!),
  release: null,
  context: {
    device: { device_id: 'worker-1' },
    os: { name: 'linux', version: null },
    app: {},
    runtime: { name: 'node', version: process.versions.node },
    user: null,
  },
  flushInterval: 5000,
  maxBatch: 30,
  maxItemsPerEnvelope: 500,
  debug: false,
});
transport.enqueue({
  type: 'event',
  name: 'raw_enqueue',
  distinct_id: 'system',
  properties: {},
  timestamp: new Date().toISOString(),
  session_id: null,
  screen: null,
});
await transport.close();
```

### Exported types

`export type * from './types.js'` re-exports the whole wire contract. The ones
you are likely to touch:

| Type | Shape / values |
| --- | --- |
| `Level` | `'debug' \| 'info' \| 'warning' \| 'error' \| 'fatal'` |
| `InitOptions` | The `init` options object (see Configuration). |
| `MetadataOptions` | `{ tags?, contexts?, extra? }` |
| `CaptureExceptionOptions` | `MetadataOptions` + `{ user?, level?, handled?, fingerprint? }` |
| `TransactionInput` | Input to `trackTransaction`. |
| `BreadcrumbInput` / `Breadcrumb` | Caller input / stored (stamped) crumb. |
| `WorkflowStatus` | `'ok' \| 'already_active' \| 'not_active' \| 'name_mismatch' \| 'invalid_name' \| 'disabled'` |
| `WorkflowResult` | `{ status: WorkflowStatus; workflowId?: string }` — return value of `startWorkflow`/`endWorkflow`/`cancelWorkflow`. |
| `ActiveWorkflow` | `{ workflowId, name, startedAt }` — return value of `getWorkflow()`. |
| `User` | `{ id?, email?, username? }` — input to `setUser`. |
| `ErrorUser` | `{ id, email, username }` — the wire shape (nulls, not absent). |
| `BeforeSend` / `BeforeBreadcrumb` | `(item, hint?) => item \| null` hooks. |
| `EnvelopeItem` | `ErrorItem \| EventItem \| IdentifyItem \| TransactionItem` |
| `Envelope`, `EnvelopeHeader`, `Context`, `Frame`, `ScopeData` | Wire structures. |
| `FetchLike`, `FetchResponse`, `ProcessLike` | Injection seams for tests. |
| `Dsn` | Result of `parseDsn`. |
| `AutoCaptureOptions` | `{ process?: ProcessLike }` |
| `ResolvedOptions` | `InitOptions` with all defaults applied. |

## Scope & metadata

Three layers contribute `tags` / `contexts` / `extra` / `user` / `breadcrumbs`,
in increasing precedence:

1. **init defaults** — `tags`/`contexts`/`extra` passed to `init` are seeded into
   the global scope at construction.
2. **scope** — the global scope (`setTag`, `setUser`, … outside any `withScope`),
   then the async-local child inside a `withScope` / `runWithAsyncScope` block. A
   child starts as a *snapshot* of its parent, so reads merge child-over-parent
   automatically and writes never escape.
3. **per-call options** — the `options` bag on `track`, `captureException`,
   `captureMessage`.

Merge rules, per key:

- `tags` — shallow-merged by key; per-call wins.
- `extra` — shallow-merged by key; per-call wins.
- `contexts` — merged by **block name**; a per-call block *replaces* the
  same-named scope block wholesale (no deep merge).
- `user` — per-call `user` wins outright; the scope's user is only used when the
  call did not supply one.
- `breadcrumbs` — always taken from the active scope's ring buffer, and attached
  to **error items only**.

Emit conventions on the wire:

- On error items, `tags` is always present (possibly `{}`), while `contexts` and
  `extra` are omitted entirely when they resolve to empty.
- On event items, all three of `tags`/`contexts`/`extra` are omitted when empty.
- `identify` items carry no scope metadata; `transaction` items take only the
  `distinct_id` fallback from the scope's user id.

```ts
init({ dsn: DSN, tags: { service: 'checkout' } });   // layer 1

setTag('region', 'eu-west-1');                        // layer 2 (global)

withScope((scope) => {
  scope.setTag('request_id', 'r-1');                  // layer 2 (child)
  captureException(err, { tags: { severity: 'high' } }); // layer 3
  // → tags: { service, region, request_id, severity }
});
// outside the block, request_id and severity are gone
```

## Framework integration

### Express

```ts
import express from 'express';
import {
  init, withScope, addBreadcrumb, captureException, trackTransaction,
  flush, close,
} from '@edraj/sauron-node';

init({
  dsn: process.env.SAURON_DSN!,
  release: process.env.GIT_SHA,
  autoCaptureUnhandled: true,
});

const app = express();

// 1. Per-request scope — must be the FIRST middleware so everything downstream
//    (including the error handler) runs inside the async-local context.
app.use((req, res, next) => {
  withScope((scope) => {
    const started = Date.now();
    scope.setUser({ id: req.header('x-user-id') ?? null });
    scope.setTag('method', req.method);
    scope.setContext('request', {
      url: req.originalUrl,
      ip: req.ip,
      user_agent: req.header('user-agent'),
    });
    addBreadcrumb({
      type: 'http',
      category: 'request',
      message: `${req.method} ${req.originalUrl}`,
      level: 'info',
    });

    res.on('finish', () => {
      trackTransaction({
        name: `${req.method} ${req.route?.path ?? req.path}`,
        op: 'http',
        duration_ms: Date.now() - started,
        status: res.statusCode < 500 ? 'ok' : 'internal_error',
        http_method: req.method,
        http_status: res.statusCode,
        url: req.originalUrl,
      });
    });

    next();
  });
});

app.get('/orders/:id', async (req, res) => {
  // withScope propagates across awaits, so captures in here carry the request
  // user/tags/breadcrumbs automatically.
  res.json(await loadOrder(req.params.id));
});

// 2. Error handler — 4 args, registered LAST.
app.use((err, req, res, _next) => {
  captureException(err, {
    tags: { route: req.route?.path ?? req.path },
    fingerprint: [`${req.method} ${req.route?.path ?? req.path}`],
  });
  res.status(500).json({ error: 'internal' });
});

// 3. Graceful shutdown — drain HTTP first, then flush the SDK.
const server = app.listen(3000);
for (const signal of ['SIGTERM', 'SIGINT'] as const) {
  process.on(signal, () => {
    server.close(async () => {
      await close();
      process.exit(signal === 'SIGINT' ? 130 : 143);
    });
  });
}
```

Because the handler above owns the exit, leave `autoShutdown` off — otherwise the
SDK's own signal handler would `process.exit()` before your server finished
draining. If you have nothing to drain, `init({ autoShutdown: true })` and drop
step 3 entirely.

### Fastify

```ts
import Fastify from 'fastify';
import {
  init, withScope, addBreadcrumb, captureException, trackTransaction, close,
} from '@edraj/sauron-node';

init({ dsn: process.env.SAURON_DSN!, autoCaptureUnhandled: true });

const fastify = Fastify();
const startedAt = new WeakMap<object, number>();

// 1. Per-request scope. Calling done() inside withScope keeps the rest of the
//    request lifecycle inside the async-local context.
fastify.addHook('onRequest', (req, _reply, done) => {
  withScope((scope) => {
    startedAt.set(req, Date.now());
    scope.setUser({ id: (req.headers['x-user-id'] as string) ?? null });
    scope.setTag('method', req.method);
    scope.setContext('request', { url: req.url, ip: req.ip });
    addBreadcrumb({
      type: 'http',
      category: 'request',
      message: `${req.method} ${req.url}`,
    });
    done();
  });
});

// 2. Errors.
fastify.setErrorHandler((err, req, reply) => {
  captureException(err, { tags: { route: req.routeOptions?.url ?? req.url } });
  reply.status(500).send({ error: 'internal' });
});

// 3. Timing.
fastify.addHook('onResponse', (req, reply, done) => {
  trackTransaction({
    name: `${req.method} ${req.routeOptions?.url ?? req.url}`,
    op: 'http',
    duration_ms: Date.now() - (startedAt.get(req) ?? Date.now()),
    http_method: req.method,
    http_status: reply.statusCode,
    url: req.url,
  });
  done();
});

// 4. Graceful shutdown.
fastify.addHook('onClose', async () => {
  await close();
});
await fastify.listen({ port: 3000 });
```

### Background jobs / workers

Anything that is not a request still deserves its own scope:

```ts
import { runWithAsyncScope, setTag, setUser, captureException } from '@edraj/sauron-node';

async function processJob(job: Job) {
  await runWithAsyncScope(async () => {
    setTag('job', job.name);
    setUser({ id: job.ownerId });
    try {
      await job.run();
    } catch (err) {
      captureException(err, { extra: { attempt: job.attempt } });
      throw err;
    }
  });
}
```

## Transport & delivery

**Batching.** Items land in a byte-bounded in-memory queue. A flush fires when
the queue reaches `maxBatch` (default 30) or every `flushInterval` ms (default
5000), whichever comes first. The interval timer is `unref`'d so it never keeps
your process alive; `flushInterval <= 0` disables it entirely. Overlapping
flushes are serialized through a promise chain, so a batch is never drained
twice.

**Queue caps.** The queue drops **oldest first** once it exceeds `maxQueueBytes`
(default 1 MiB), measured on each item's serialized size — a stalled ingest can
never grow memory without bound. Separately, no single envelope carries more than
1000 items (matching the server limit); a larger backlog is split across
consecutive requests within one flush.

**Offline persistence.** With `offlineDir` set, every queued item is also written
to a sequence-named FIFO file (`0000000000000007.env.json`) in that directory,
and a fresh process reloads them on construction — at-least-once delivery across
restarts. Files are unlinked only when their batch is committed (delivered or
intentionally dropped); a corrupt/partial file is discarded rather than wedging
the queue. Writes are best-effort and never block the send path. Off by default.

**Compression.** Bodies strictly larger than `gzipThresholdBytes` (default 1024)
are gzipped with `node:zlib` and sent with `Content-Encoding: gzip`. A negative
threshold disables compression.

**Retry policy.** Per envelope, with `maxRetries` (default 3) retries *after* the
first attempt:

| Response | Behavior |
| --- | --- |
| 2xx | Committed; persisted files unlinked. |
| 408, 429, any 5xx | Retried. On 429 a `Retry-After` header (delta-seconds or HTTP-date) sets the delay. |
| Network error / thrown `fetch` | Retried. |
| 413 | **Not** retried as-is — the working envelope size is halved and the batch re-buffered for the next flush. A *single* item that still 413s is dropped. |
| 401, 403 | Batch committed and the SDK **disables itself permanently** for the process. |
| 400, 404, any other non-2xx | Dropped without retry. |

Backoff is exponential with equal jitter: `retryBaseMs * 2^attempt` (base 200 ms),
halved and re-jittered, and every individual sleep — including a `Retry-After`
delay — is capped at 30 s. After the last retry the batch is **re-buffered**, not
discarded, so it can go out on a later flush or after a restart. `flush()` never
rejects.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing arrives, no errors logged | The ingest is not exposed at `/api/{environment_id}/envelope` on the host root. A DSN cannot express a path prefix, so a proxy that serves Sauron under e.g. `/sauron/` silently 404s and the SDK drops the batch. | Expose ingest at `/api/{environment_id}/envelope` on the DSN host root. |
| Nothing arrives, nothing happens at all | Capture calls ran before `init` or after `close()` — they are silent no-ops. | Check `getClient() !== null`. |
| Events stop after a while, one warning logged | The ingest returned 401/403; the SDK disabled itself for the process. | Fix the public key in the DSN and restart. Set `debug: true` to see `auth failed (401); disabling SDK`. |
| Events lost when the process exits | Buffered items had not flushed; the flush timer is `unref`'d and does not hold the loop open. | `await close()` before exiting, or `init({ autoShutdown: true })`. |
| Items lost on a hard crash / container kill | The in-memory queue is not persisted by default. | Set `offlineDir`. |
| Errors missing but events arrive | `sampleRate < 1` — it applies to `captureException` only. | Raise `sampleRate` (default 1). |
| No breadcrumbs on captured errors | `maxBreadcrumbs: 0`, `beforeBreadcrumb` returned `null`, or the crumbs were added in a different `withScope` than the capture. | Add crumbs and capture inside the same scope. |
| Scope data leaking between requests | Metadata was set on the global scope instead of a per-request child. | Wrap each request in `withScope` / `runWithAsyncScope`. |
| `Error: [sauron] global fetch is unavailable` | Node < 18. | Upgrade to Node >= 18 or pass `fetchImpl`. |
| `DsnError: [sauron] invalid DSN: …` | Malformed DSN, wrong protocol, or a password component. | Use `https://<public_key>@<host>/<environment_id>` with no secret. |
| Want to see what the transport is doing | — | `init({ debug: true })` — decisions are logged to `console.warn` with a `[sauron]` prefix. |

## Development

```bash
npm install
npm run build       # tsc -p tsconfig.build.json
npm test            # vitest run
npm run test:watch  # vitest
npm run typecheck   # tsc --noEmit
```

## License

LGPL-3.0-only — GNU Lesser General Public License v3.0. LGPLv3 applies on top of
the GNU GPL v3, whose text ships alongside it in `COPYING`.
