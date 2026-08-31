# Server-side view cache + honest freshness — design

**Status:** slices 2, 3, 4, 6 built and verified. Slice 5 MEASURED AND NOT
JUSTIFIED (see below). Slice 1 outstanding (operator change on the deployment).
**Date:** 2026-08-31
**Supersedes:** nothing. Generalises the pattern proven in
`bins/sauron-api/src/overview_cache.rs` (Overview only) and
`routes/active_users.rs` (one route) to the rest of the analytics surface.

## Why

Three production reports in one week, all the same shape:

- `GET /v1/projects/{id}/active-users` — 503, "taking too long".
- `POST /v1/apps/{id}/rollups/refresh` — 503, "way slow".
- "The dashboard should show warm data immediately, say when it was captured,
  and tell me it is fetching."

The first two are the same defect class as the one `overview_cache` was built
to fix: an aggregate whose cost scales with **retained data** rather than with
the caller's window, run **on the request path**, behind a 60 s
`TimeoutLayer` (`main.rs:53`) that maps a timeout onto `SERVICE_UNAVAILABLE`.
Measured for Overview before that module existed: top-issues 5.7 s, top-events
6.1 s, series 7.0 s, active-users 13.4 s, totals past the budget — i.e. a 503
with nothing behind it.

The third is the same problem stated as a product requirement.

The fix already exists and is proven; it is simply not general. This spec
makes it general.

## What exists today

| Layer | State |
|---|---|
| `overview_cache.rs` (1,037 lines) | Full Redis cache + background recompute + SSE. **Overview page only.** |
| `routes/active_users.rs` | Redis cache, `computed_at`, single-flight — but the cold miss still computes **on the request path**. |
| `lib/stores/view-cache.ts` + `CachedView` | Browser, in-memory only. 27 of 43 pages. Dies on reload. |
| Freshness shown to the user | 9 pages. |
| Endpoints returning `computed_at` | 2 (`active_users`, `retention`). |

The browser cache makes *navigation* instant. It does nothing for a cold load,
because it is a `Map` at module scope that dies with the tab. Only a server
cache makes the first paint fast, and it does so for every user and every tab.

## Scope

**In:**

1. Extract `overview_cache`'s mechanism into a reusable server-side view cache.
2. Adopt it on the slow analytics routes, `/active-users` first.
3. Return a uniform envelope carrying `computed_at`.
4. One dashboard freshness component: "as of <time>" + "Updating…", reading the
   server's `computed_at` where present and the browser's `storedAt` otherwise.
5. Redis `maxmemory` + eviction policy as a deployment precondition.

**Out (explicitly):**

- **Delta / incremental fetching.** Decided against: an aggregate (p95, DAU,
  retention cohort, funnel) cannot be updated by fetching "just the new rows" —
  it must be recomputed. Only append-only lists could use it, and those are not
  the slow pages.
- **Persisting the browser cache** to `localStorage` / `sessionStorage` /
  IndexedDB. Considered and rejected in favour of the server cache: it delivers
  more speed (cold loads, all users) with less exposure. These payloads carry
  error bodies, breadcrumbs, user traits and IPs; persisting them writes an
  at-rest copy outside every retention window and RBAC check, readable by
  anything on the origin. The prohibition in `view-cache.ts` stands.
- Rewriting `overview_cache`'s SSE fan-out. Section 4 keeps polling for the
  newly-adopted routes; SSE stays Overview-only until there is a reason.

## Architecture

Extract the mechanism, keep the semantics:

```
GET <any cached analytics route> ─▶ authorize ─▶ Redis GET ─┬─ fresh  ─▶ 200 {state:"fresh",     computed_at, data}
                                                            ├─ stale  ─▶ 200 {state:"stale",     computed_at, data} ─┐
                                                            └─ miss   ─▶ 200 {state:"computing", data:null}         ─┤ enqueue
                                                                                                                     │
                                        background worker (bounded permits) ◀───────────────────────────────────────┘
```

The request does three cheap things — authorize, read Redis, maybe enqueue — so
**it cannot reach the 60 s timeout regardless of how slow the aggregate is.**
That property is the whole point; any change that puts the aggregate back on the
request path reintroduces the 503.

### Two numbers, never conflated

- **Freshness threshold** — how old a value may be before a recompute is *also*
  kicked. This is the product contract: "numbers may be up to N old, and the
  page says how old."
- **Redis TTL** — how long the entry survives at all. Must be **much** longer
  than the freshness threshold. If they were equal, the first request after the
  threshold would find nothing and render a skeleton behind a slow aggregate —
  exactly the behaviour this removes. Overview uses 2 min / 24 h.

### The cache key is the safety-critical part

Inherited verbatim from `active_users.rs::cache_key`, whose comment says to
treat deviation as Critical in review:

- The fingerprint must be **injective by construction**. Serialise a struct to
  JSON (self-delimiting); never join variable-length parts into a string. The
  entry holds a whole payload plus app names, so a collision is a **cross-tenant
  data leak**, not a staleness bug.
- The key uses the **resolved** scope, never the requested token. That is what
  stops a caller with app-wide reach (`All`) sharing an entry with one holding
  only env-X (`Subset([X])`).
- Every id the response varies on goes in — including `environment_id`, which
  the axios interceptor adds to requests but which appears in no handler
  argument.

### Envelope

```jsonc
{ "state": "fresh" | "stale" | "computing",
  "computed_at": "2026-08-31T09:12:04Z" | null,
  "data": { ... } | null }
```

`data` is nullable **only** when `state == "computing"`. Clients already handle
this shape for Overview sections (`get_section` in the HTTP tests polls it).

## Dashboard

One component, `<Freshness>`, rendering two independent facts:

- **when the data was computed** — server `computed_at` if the endpoint returns
  one, else the browser's `CacheEntry.storedAt` (already recorded; currently not
  exposed through `CachedView`).
- **whether a refresh is in flight** — `CachedView.revalidating`, or
  `state == "computing"`.

They must stay visually distinct. "Updating…" next to a timestamp must not
imply the figure is nearly current: `/active-users` can serve an answer Redis
has held for hours, and a browser-fetch timestamp would report that as seconds
old. Prefer the server's stamp wherever one exists — that is the honest number.

`CachedView` gains `fetchedAt` (from `storedAt`, via the existing `peek` that
does not count as a use) and passes through a server `computed_at` when the
payload carries one.

## Deployment precondition

Redis is currently `maxmemory 0` with `maxmemory_policy noeviction`. Under that
config a full Redis does not evict — it **errors on writes**, which would take
out sessions, rate limiting and the ingest stream, not merely the cache. This
deployment has already had one Redis RAM incident (an orphaned dead-letter
stream at 99% of usage). Set `maxmemory` and an LRU policy **before** the cache
broadens. Current usage is 1.67 MB and entries are small JSON, so the headroom
required is modest; the risk is the policy, not the volume.

## Slices

1. **Redis config + `maxmemory` precondition documented.** No code. OUTSTANDING —
   an operator change on the deployment, not a repo change.
2. **Extract the mechanism** — DONE. `bins/sauron-api/src/view_cache.rs` holds
   the freshness decision, envelope, single-flight claim, permit ceiling and the
   Redis get/put pairs. `overview_cache` delegates to it and its tests pass
   unchanged. The per-route `enqueue` deliberately stays with each route: what
   happens after a recompute genuinely differs (only Overview fans out on SSE).
3. **`/active-users` onto it** — DONE. The JSON route returns the envelope and
   never awaits the aggregate; the cache key gained a `v2` because the stored
   document shape changed. The CSV route deliberately still computes on the
   request path: a downloaded file has nowhere to put "not ready", and a
   download is a rare deliberate action where a page load is neither.
4. **`CachedView.fetchedAt` + `<Freshness>`** — DONE for the 15 pages that have
   a `RefreshButton` to anchor the chip to. 10 more hold a `CachedView` but have
   no header control; they need a slot deciding.
5. **Remaining slow analytics routes** — MEASURED, and the answer is *almost
   none*. Fifteen of the heaviest repo functions already have a rollup fast
   path (`overview_totals`, `user_stats`, `performance_summary`/`_series`,
   `session_stats`, `event_series`, `error_series`, `screen_list`,
   `journey_graph`, `top_events`, …), and Overview and `/active-users` are now
   on the server cache. The only genuinely slow routes left are the three
   cross-tier timeseries endpoints, measured on the 84 GB dev set over a 7-day
   window: **transactions 21.9 s, events 7.6 s, errors 4.0 s**. But **no
   dashboard page and no SDK calls any of them** — they are referenced only by
   the env-scoping allowlist and two design docs. Caching them would be a
   breaking wire change on public API surface, for endpoints nothing we ship
   calls, so it is not worth doing now. Revisit if a caller appears; the 21.9 s
   is a real liability for any external consumer.
6. **The pages lacking `CachedView`** — DONE. All seven converted and driven
   in the browser: Transactions, Purge, MonitorDetail, IssueDetail, Inspector,
   FunnelBuilder, Environments. Every data page in the app now caches; the only
   pages without it are the nine that should not have it (Login, Register,
   Docs, the password flows, Onboarding, Unsubscribe, AdminIndex).

   Three things the conversions turned up, worth carrying into any future one:

   - **Environments already cached**, by using the lower-level `viewCache`
     directly rather than the `CachedView` class — a grep for the class name
     missed it. Its conversion bought the generation guard and the freshness
     stamp, not caching.
   - **Side effects done inline on a successful fetch must move to an `$effect`
     on the payload**, or a cache hit — which runs no fetch at all — silently
     skips them. Three cases: Transactions' expanded-row reset, MonitorDetail's
     interval reseed (which exists precisely because the router reuses the
     component across monitor ids), and Environments' `apply`.
   - **Load errors and action errors need separate variables.** Four pages set
     one `error` from both their read and their mutations; with the read's
     error derived from the view, the mutation paths need their own state or
     they cannot report anything. They have different lifetimes anyway — an
     action's message must survive a background revalidate.

   Two pages deliberately cache only part of themselves. **FunnelBuilder**
   caches the 90-day event CATALOGUE but not the builder: its load seeds
   `steps`/`picked`/`result`, which the user then edits, so caching those would
   either clobber an edited funnel on return or leave the builder empty on a
   hit. **Inspector** caches its six values as ONE payload because they are one
   dependent chain — the policy decides whether scans are read, and the newest
   succeeded scan decides which findings are; cached separately, a hit on one
   could pair with a miss on another and show a policy beside findings from a
   different scan.
