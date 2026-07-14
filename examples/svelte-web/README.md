# Sauron — Web SDK Demo (Svelte + Vite)

A small single-page **Vite + Svelte 5 (TypeScript)** app that showcases the
[`@sauron/browser`](../../sdks/js) SDK end-to-end. Click the buttons to push
errors and product-analytics events to a running Sauron **ingest gateway**, then
open the dashboard to see the grouped issue + events appear.

It depends on the SDK via a local path (`"@sauron/browser": "file:../../sdks/js"`),
so no publish/registry step is needed.

## What it demonstrates

- `Sauron.init({ dsn, environment, release })` — with a DSN you can edit in the
  UI (persisted to `localStorage`) and re-init on demand.
- Automatic capture of **uncaught errors** and **unhandled promise rejections**
  (installed by `init`).
- `captureException`, `captureMessage`, `track`, `identify`, and `addBreadcrumb`.
- **Screen tracking (v0.2.0)** — `Sauron.setScreen('Home')` runs right after
  `init` (in `src/lib/sauron.ts`), and the **setScreen (navigate)** action card
  toggles `Home ⇄ Checkout`. Each change emits a `$screen` view and tags
  subsequent events/errors with the active screen (see `Sauron.getScreen()`).
- A one-click **cohort simulator** (see below) that lights up the analytics
  screens.
- A client-side **activity log** echoing each call (the SDK batches/gzips and
  delivers envelopes in the background — this panel is local feedback only).

## Showcase funnels, journeys & performance

The single-event buttons are great for Issues/Events, but a single user clicking
buttons is one `distinct_id` — a flat, single-path funnel. The **Run showcase**
panel (top of the page) fixes that: one click drives the SDK through a synthetic
e-commerce cohort — **~120 users by default** (editable, capped at 500), each
switched via `setUser` so their events keep their own `distinct_id`.

Each synthetic user walks the funnel
`product_viewed → product_added_to_cart → checkout_started → payment_info_entered → checkout_completed`
with realistic **drop-off**, branches into side events (`search_performed`,
`viewed_recommendations`, `applied_coupon`) for the journey graph, and emits a
spread of `trackTransaction()` calls (route load, `GET /api/products`,
`POST /api/checkout`, resource loads) with skewed latencies — so percentiles and
latency colors look real. The panel renders the resulting funnel inline as it
finishes.

After a run, open the **dashboard → Web Demo app**:

- **Funnels** — prefilled with the first three steps; add the rest to see the
  full 5-step conversion.
- **Journeys** — the branching Sankey of paths through the cohort.
- **Performance** — p50/p95/p99 and latency badges over the transactions.

The simulator is pure logic + an injected sink (`src/lib/showcase.ts`), unit
tested with Node's built-in runner:

```bash
node --test src/lib/showcase.test.ts
```

## Run

```bash
npm install
npm run dev       # http://localhost:5173
```

Then, in the app:

1. The SDK auto-initializes from the pre-filled DSN on load (status pill → **Connected**).
2. Click any action button.
3. Open the **Sauron dashboard → Web Demo app → Issues / Events**. Events are
   flushed every ~3s (and on page unload via `sendBeacon`), so give it a moment.

Build (type-check + production bundle):

```bash
npm run build     # svelte-check && vite build  → dist/
npm run preview   # serve the built bundle
```

## DSN configuration

The DSN is `http://<public_key>@<host>/<project_id>`. The public key is a
non-secret, write-only credential — safe to ship in client code.

**Default (local dev ingest, port 8091):**

```
http://pk_4cf799b01ea53473661c82827a75cb87@localhost:8091/3ccbaa22-3750-477c-a330-faca235d7337
```

This points at a real **"Web Demo"** app on the dev ingest running on `:8091`.

**Docker Compose ingest runs on a different port.** In `docker-compose.yml` the
ingest publishes **`:8081`** (dashboard `:3000`, API `:8080`). To demo against
the compose stack, change the host in the DSN input to `localhost:8081`:

```
http://pk_4cf799b01ea53473661c82827a75cb87@localhost:8081/3ccbaa22-3750-477c-a330-faca235d7337
```

(The public key / project id above belong to the dev ingest's seeded app; a
fresh compose stack will have its own app — paste that app's DSN instead.)

You can paste any DSN into the header input and click **Init / Reconnect**; the
value is saved to `localStorage`. **Reset** restores the dev default.

## Notes

- The ingest accepts cross-origin requests (`Access-Control-Allow-Origin: *`),
  so the browser can POST envelopes from the Vite dev server directly.
- The wire endpoint is `POST /api/{project_id}/envelope` with an
  `X-Sauron-Key: <public_key>` header — handled entirely by the SDK.
