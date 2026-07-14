# @sauron/browser

Browser SDK for **Sauron** — error reporting + product analytics in one small
package. Captures uncaught errors and unhandled promise rejections
automatically, records breadcrumbs, exposes `track()` / `identify()`, and
batches → gzips → queues envelopes (offline-safe) before POSTing them to the
Sauron ingest gateway.

- Zero-config auto-instrumentation: `window.onerror`, `onunhandledrejection`,
  `console`, DOM clicks, `fetch`, `XMLHttpRequest`, and SPA history navigation.
- One runtime dependency (`fflate`, used only as a gzip fallback).
- Ships ESM + CJS + type definitions. `sideEffects: false`, tree-shakeable.

## Install

```bash
npm install @sauron/browser
```

## Quick start

```ts
import { Sauron } from '@sauron/browser';

Sauron.init({
  dsn: 'https://pk_test@ingest.sauron.dev/42',
  environment: 'production',
  release: 'web@1.4.2',
  sampleRate: 1,          // fraction of errors to send
  maxBreadcrumbs: 50,
  beforeSend(item) {      // PII escape hatch — return null to drop
    return item;
  },
});

Sauron.identify('u_123', { plan: 'pro' });
Sauron.track('checkout_completed', { cart_value: 42.5 });

try {
  doRiskyThing();
} catch (err) {
  Sauron.captureException(err);
}
```

## API

| Function | Description |
| --- | --- |
| `init(options)` | Initialize the SDK (idempotent). |
| `captureException(err, hint?)` | Report an exception or any thrown value. |
| `captureMessage(msg, level?)` | Report a plain message. |
| `track(name, props?)` | Record a product-analytics event. |
| `trackTransaction(input)` | Record a performance transaction (navigation / http / screen load). |
| `identify(id, traits?)` | Associate the session with a user. |
| `addBreadcrumb(crumb)` | Manually add a breadcrumb. |
| `setUser(user \| null)` | Set or clear the current user. |
| `flush(timeoutMs?)` | Send everything pending; resolves `false` on timeout. |
| `close(timeoutMs?)` | Flush, then restore all patched globals. |

### `init` options

```ts
Sauron.init({
  dsn: string,               // https://<public_key>@<host>/<project_id>
  environment?: string,      // default "production"
  release?: string,          // e.g. "web@1.4.2"
  sampleRate?: number,       // default 1
  maxBreadcrumbs?: number,   // default 50
  beforeSend?: (item, hint) => item | null,
  beforeBreadcrumb?: (crumb, hint) => crumb | null,
  transport?: {
    flushIntervalMs?: number, // default 5000
    maxBatch?: number,        // default 30
    maxQueueBytes?: number,   // default 1048576
  },
  performance?: boolean,     // auto-capture perf transactions (opt-in), default false
  debug?: boolean,           // default false
});
```

## Wire contract

The SDK POSTs a canonical envelope to `POST /api/{project_id}/envelope`:

```
Content-Type: application/json
Content-Encoding: gzip        # only when compressed (payloads ≳ 1 KB)
X-Sauron-Key: <public_key>
```

On page unload, the pending batch is delivered via `navigator.sendBeacon` to
`POST /api/{project_id}/envelope?k=<public_key>` (uncompressed JSON blob).

The envelope shape (`header` + `context` + `items[]`) is identical across the
JavaScript, Flutter, and Rust implementations — see `src/types.ts`.

## Development

```bash
npm install
npm run typecheck   # tsc --noEmit
npm run build       # tsup -> dist/ (esm + cjs + d.ts)
npm test            # vitest
```

## License

AGPL-3.0-only — GNU Affero General Public License v3.0.
