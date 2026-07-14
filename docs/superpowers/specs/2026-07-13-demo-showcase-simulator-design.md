# Demo apps: one-click funnel / journey / performance showcase

**Date:** 2026-07-13
**Status:** Approved design (pending spec review)
**Applies to:** `examples/svelte-web`, `examples/flutter-app`

## Problem

The two SDK demo apps (`examples/svelte-web` for web, `examples/flutter-app` for
mobile) each fire **one event per button click** — errors, a couple of `track()`
events, `identify`. That exercises error grouping and the Events/Issues screens,
but it leaves the dashboard's **Funnels**, **Journeys**, and **Performance**
screens effectively empty:

- **Funnels** (`repo::funnel`) and **Journeys** (`repo::journey_links` /
  `journey_nodes`) both group by `analytics_events.distinct_id`, time-ordered. A
  single demo user clicking buttons is **one** `distinct_id` → a single linear
  path with 100% conversion and no branching. Nothing interesting to show.
- **Performance** (`repo::*` percentiles over `transactions`) needs a spread of
  `trackTransaction()` calls with varied latency to make p50/p95/p99 and the
  LatencyBadge colors meaningful. The demos never call `trackTransaction()`.

We want each demo to **showcase** those three screens with realistic,
non-trivial data — driven through the *real* SDK, client-side (these apps exist
to demonstrate the SDK doing the work; this is not a backend seed).

## Approach

Add a **one-click cohort simulator** to each demo: a "Showcase" panel with a
**Run showcase** button that drives the SDK through a synthetic but realistic
e-commerce cohort. Chosen over an interactive step-through (which only produces
one user's linear path) because only a multi-user cohort with drop-off and
branching makes Funnels/Journeys/Performance render meaningfully.

### Why `setUser`, not `identify`

Both SDKs derive the outgoing `distinct_id` from the scope user id:

- JS `client.getDistinctId()` → `scope.getUser().id` if set, else anonymous id.
- Flutter `Scope.distinctId` → `user?.id`.

So calling **`setUser({ id: <simId> })`** before a synthetic user's event burst
switches the `distinct_id` on every subsequent `track()` / `trackTransaction()`
**cleanly**, using only the public API:

- No `identify()` — which would also emit an identify item and call
  `repo::insert_identity` (anon→distinct alias) N times, polluting the Persons
  screen and risking person-merge.
- The backend stores each event's own `distinct_id` at ingest
  (`process.rs` → `analytics_events.distinct_id`), and the funnel/journey SQL
  reads that column directly, so N synthetic users become N funnel entrants /
  journey partitions.

Before the run, the driver **captures the current scope user** (usually `null`
in the web demo, or whatever the user last set via the manual `identify` button)
and, in a `finally`, **restores exactly that** with `setUser` — so later manual
actions aren't attributed to the last synthetic user, whether or not the run
throws partway.

## The scenario (identical shape in both apps)

Kept structurally identical across web and mobile so the two apps produce
matching dashboards. Not shared code (different languages) — a mirrored data
structure + driver in each app.

### Funnel (5 ordered events)

Reuses the existing `checkout_completed` vocabulary so manual clicks and the
showcase reinforce **one** funnel:

1. `product_viewed`
2. `product_added_to_cart`
3. `checkout_started`
4. `payment_info_entered`
5. `checkout_completed`

Because Funnels prefills the **top 3 events by count**, this scenario makes the
Funnels screen show `product_viewed → product_added_to_cart → checkout_started`
immediately, with the full 5-step funnel one click away.

### Retention curve (drop-off)

Cumulative retention across the 5 steps, approximately:

`[100%, 65%, 43%, 28%, 20%]`

Each synthetic user walks the funnel and stops emitting once they
probabilistically drop. Retention is **monotonically decreasing** (a testable
invariant). Exact per-run counts vary with the PRNG.

### Branching side-events (for Journeys)

So the Journeys Sankey fans out instead of being a single line, a fraction of
users emit extra events interleaved with the funnel:

- `search_performed` — entry event for ~40% of users (before `product_viewed`).
- `viewed_recommendations` — ~30%, between view and cart.
- `applied_coupon` — ~20%, between checkout_started and payment.

### Performance transactions (`trackTransaction`)

Interleaved per user so percentiles are populated:

| op | name | latency spread (ms) | notes |
|----|------|--------------------|-------|
| `navigation` | `/products` route load | ~200–1400 | web: navigation; every user |
| `http` | `GET /api/products` | ~80–1800 | httpStatus 200; ~3% → 500 |
| `http` | `POST /api/checkout` | ~150–2600 | only cart+ users; ~4% → 500 |
| `resource` | `bundle.js` / asset | ~40–600 | ~50% of users |
| `screen_load` | `ProductList` / `Checkout` | ~120–1600 | **mobile only** |

Latencies are drawn from a right-skewed distribution (e.g. `min + (max-min) *
random^k` with `k≈2.2`) so p50 sits low and p95/p99 stretch out — enough for the
LatencyBadge to show green/amber/red (scale: green <1s, amber <3s). A small
fraction carry `status: 'error'` / `httpStatus: 500` so error/crash-free rates
are non-zero.

## Volume & pacing

- Default **120 synthetic users**, editable in the UI, capped at **500**.
- Per user: ≤5 funnel events + 0–3 side-events + ~2–4 transactions ≈ **7
  envelopes** → ~800–1000 envelopes/run at the default.
- The driver **yields periodically** (every ~10 users: `await` a
  timer/microtask) so the UI thread stays responsive and the transport batches
  naturally.
- The driver calls **`flush()`** at the end so data reaches the dashboard within
  a couple of seconds rather than waiting on the batch interval.
- Re-runs use a fresh `runId` (a per-run counter/timestamp component in the sim
  ids) so repeated runs **accumulate** distinct users rather than colliding.

## Component design

Each app splits into a **pure scenario planner** (deterministic-shaped, no SDK
calls — testable) and a thin **driver** (walks the plan, calls the SDK, reports
progress).

### Web — `examples/svelte-web`

- **New `src/lib/showcase.ts`:**
  - Scenario constants (funnel steps, retention curve, side-event rates,
    transaction specs).
  - `planUser(rng, index, runId): UserPlan` — pure; returns the ordered list of
    `{kind: 'event'|'txn', ...}` actions for one synthetic user, honoring the
    retention curve and side-event rates. No SDK calls.
  - `runShowcase(opts, onProgress): Promise<ShowcaseSummary>` — the driver:
    loops `count` users, `setUser` → walk `planUser` actions via
    `Sauron.track` / `Sauron.trackTransaction`, yields periodically, restores
    identity, `Sauron.flush()`, returns a summary (per-step counts, txn count).
  - Accepts an injectable "sink" (defaults to the real `Sauron` facade) so tests
    can drive it with a fake and assert emitted items.
- **`src/App.svelte`:** a new "Showcase" `<section>`/card above or below the
  existing action grid — a Run button, a numeric users input (default 120), a
  progress line (`x / N users · M events`), disabled while running and gated on
  `ready` like the other actions. Progress + the final summary echo into the
  existing `ActivityLog` via the `activity` store.

### Mobile — `examples/flutter-app`

- **New `lib/showcase.dart`:**
  - Mirrored scenario constants + `planUser(...)` (pure) + `runShowcase(...)`
    driver using `Sauron.setUser` / `Sauron.track` / `Sauron.trackTransaction` /
    `Sauron.flush`, with `screen_load` transactions added.
  - Driver takes an `onProgress` callback and an optional injectable emitter for
    tests.
- **`lib/main.dart`:** a new `_SectionHeader('Showcase')` block with an
  `_ActionTile`-style card: a Run button, a count field (default 120), a
  `LinearProgressIndicator` while running, and progress/summary lines pushed into
  the existing `_log` via `_record`. Restore `setUser` to the configured
  `distinct_id` afterward.

## Docs

Update both READMEs (`examples/svelte-web/README.md`,
`examples/flutter-app/README.md`): what the Showcase button does, the ~120-user
default, and which dashboard screens to open afterward (Funnels — prefilled with
the first 3 steps; Journeys; Performance).

## Testing

- **Web (vitest):** unit-test the planner + driver against a fake sink:
  - Aggregate per-step funnel counts over many users are **monotonically
    non-increasing** and step 0 == count.
  - Every emitted transaction has a known `op` and a positive `durationMs`.
  - The captured pre-run scope user is restored after `runShowcase` (sink
    observes it as the final `setUser`, including the `null` case).
- **Mobile (flutter test):** mirror the same invariants against an injected
  emitter (monotonic funnel counts, well-formed transactions, identity restored).
- **Build gates:** web `tsc`/`vite build` clean; Flutter `flutter analyze` clean
  and `flutter test` green. (Existing SDK test suites are untouched.)

## Out of scope

- No SDK changes — both SDKs already expose `setUser`, `track`,
  `trackTransaction`, `flush`. This is demo-app + docs only.
- No backend seed script (docker-compose seeding already exists separately).
- No new dashboard screens — the existing Funnels/Journeys/Performance screens
  consume this data as-is.
