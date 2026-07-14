# @sauron/node — server-side example

A tiny, copy-pasteable Node backend that exercises the full `@sauron/node`
lifecycle: `init` → `identify` → `track` → `captureException` → `flush`/`close`.

## Run

```bash
cd examples/node-server
npm install
SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
```

The DSN is read from the `SAURON_DSN` environment variable. Optional:

| Env var          | Default       | Purpose                        |
| ---------------- | ------------- | ------------------------------ |
| `SAURON_DSN`     | *(required)*  | Your project DSN.              |
| `NODE_ENV`       | `development` | Passed as `environment`.       |
| `SAURON_RELEASE` | `1.0.0`       | Passed as `release`.           |

If the Sauron ingest is not running the SDK simply buffers and the POST fails
in the background — the process still exits cleanly.

## Typecheck

```bash
npm run typecheck   # tsc --noEmit against the shipped @sauron/node types
```

## What it does

```ts
import { init, identify, track, captureException, flush, close } from '@sauron/node';

init({ dsn: process.env.SAURON_DSN!, environment: 'production', release: '1.0.0' });

identify('user-42', { plan: 'pro' });
track('order_completed', 'user-42', { total: 42.5, currency: 'USD' });

try {
  doWork();
} catch (err) {
  captureException(err, { user: { id: 'user-42' }, tags: { area: 'checkout' } });
}

await close(); // flushes buffered items, then stops the background timer
```

See `sdks/node/README.md` for the complete API.
