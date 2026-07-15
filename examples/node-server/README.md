# @sauron/node — server-side example

A tiny, copy-pasteable Node backend that exercises the `@sauron/node` **0.3.0**
surface: a per-request `withScope`, `addBreadcrumb` before a captured exception,
and one `trackTransaction` — plus `identify` / `track` and `flush` / `close`.

## Run

```bash
cd examples/node-server
npm install
SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
```

The DSN is read from the `SAURON_DSN` environment variable. If it is **unset**,
the client is never initialized, every dispatch call is a no-op, and the process
still exits `0` (disabled mode) — handy for a smoke run without an ingest.

| Env var          | Default       | Purpose                             |
| ---------------- | ------------- | ----------------------------------- |
| `SAURON_DSN`     | *(optional)*  | Your project DSN; unset ⇒ disabled. |
| `NODE_ENV`       | `development` | Passed as `environment`.            |
| `SAURON_RELEASE` | `1.0.0`       | Passed as `release`.                |

If the Sauron ingest is not running the SDK simply buffers and the POST fails in
the background — the process still exits cleanly.

## Typecheck

```bash
npm run typecheck   # tsc --noEmit against the @sauron/node 0.3.0 types
```

> `tsconfig.json` maps `@sauron/node` to the SDK source (`../../sdks/node/src`)
> so the example typechecks against the shipped 0.3.0 API without a prior
> `npm run build` of the SDK.

## What it does

```ts
import {
  init, identify, track, captureException,
  trackTransaction, addBreadcrumb, withScope, setUser, setTag,
  flush, close,
} from '@sauron/node';

if (process.env.SAURON_DSN) {
  init({ dsn: process.env.SAURON_DSN, environment: 'production', release: '1.0.0' });
}

identify('user-42', { plan: 'pro' });
track('order_completed', 'user-42', { total: 42.5, currency: 'USD' });

// Isolated per-request scope: this user/tag/breadcrumbs never leak to a
// concurrent request, and attach to any error captured inside the callback.
withScope(() => {
  setUser({ id: 'user-42', email: 'ada@example.com' });
  setTag('route', 'POST /checkout');
  addBreadcrumb({ category: 'payment', message: 'charging card', level: 'info' });
  try {
    doWork();
  } catch (err) {
    captureException(err, { tags: { area: 'checkout' } });
  }
});

// One timed operation as a performance transaction.
trackTransaction({ name: 'POST /checkout', op: 'http', http_status: 500, duration_ms: 12.5 });

await close(); // flushes buffered items, then stops the background timer
```

See `sdks/node/README.md` for the complete API.
