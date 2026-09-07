# Global force refresh

**Status:** approved design, not yet implemented
**Date:** 2026-09-07

## Problem

The dashboard caches in two independent layers and neither can be fully bypassed
from the UI.

**Client:** `viewCache` + `CachedView` hold every page's payload for
`DEFAULT_FRESH_MS` (60s). 34 pages use `CachedView`; **21 have a
`RefreshButton`** that passes `force = true`, and **13 have no refresh control
at all** — `Members`, `Storage`, `Roles`, `Projects`, `Environments`,
`Inspector`, `Purge`, `Retention`, `SettingsApp`, `SourceMaps`, and the
`IssueDetail` / `MonitorDetail` / `PersonProfile` detail pages.

**Server:** a Redis result cache (`view_cache.rs`, and `overview_cache.rs` on
top of it) backs 10 read sites across four route modules. A client-side `force`
does not reach it: the request goes out, the server answers from Redis, and the
user sees the same numbers. Clicking Refresh on those pages is honest about
having re-requested and misleading about having re-computed.

There is no single control that means "nothing on this screen is cached".

## Decisions

Settled during brainstorming; not open in implementation.

1. **One global button in the shell**, present on every page, that also busts
   the server cache. Not per-page buttons, and not client-cache-only.
2. **Asynchronous.** A force enqueues a recompute and returns immediately; the
   button stays in its loading state and the client polls until each section
   reports fresh. No request ever waits on the aggregate — that is the property
   `view_cache.rs` exists to preserve, and abandoning it puts the 30s
   `TimeoutLayer` 503s back.
3. **Everyone may force, behind a shared per-app-and-scope cooldown.** Not a
   permission. Ten people clicking costs one recompute.
4. **Reach:** every `CachedView` the current page has loaded, plus a
   `viewCache.clear()` so no *other* view serves a pre-refresh snapshot on the
   next navigation. Server recompute is limited to what the current page reads.
5. **Approach: `CachedView` self-registration**, so the 34 pages are untouched
   and the 13 with no control today get the feature for free.
6. **An explicit force clears the failure marker**, so a failure-suppressed
   section is genuinely retried. The cooldown bounds retries of a broken query.

## What already exists

Three pieces of this are already shipped, and the design reuses rather than
reinvents them:

- **`ForceQuery { force: bool }`** (`routes/analytics.rs:947`) is already wired
  into the five Overview section GETs. Its doc records the rule that matters:
  **`force` must never reach the cache key**, because a forced and an unforced
  read of the same selection are the same question. Keying on it gives Refresh
  its own permanently-cold cache — a bug whose only symptom is that refreshing
  is always slow, which reads as correct behaviour.
- **`view_cache::read(redis, key, policy, force)`** already takes the `force`
  bool. `active_users` (×2), `funnels` and `admin` pass a hard-coded `false`.
- **`api.interceptors.request`** already injects URL-scoped query params
  (`computeScopeParams`), which is the exact shape the client needs.

### An existing hole this closes

`overview_totals` and its four siblings accept `Query<ForceQuery>` and pass
`f.force` through with no gate beyond the normal read authorization — so **any
user who can read Overview can already trigger a recompute** with
`?force=true`. Meanwhile `POST /overview/refresh` gates the same capability on
`org:manage`, with a comment calling it "a self-DoS button" in the hands of
every `event:read` holder.

The two cannot both be right. Decision 3's cooldown resolves it in the
direction that keeps the feature: the capability stays open, and the cost is
bounded by time rather than by role. Applying the cooldown to the five existing
Overview GETs is therefore part of this work, not a follow-up.

## Client design

### `CachedView` remembers its last call

`load(key, fetcher, force)` takes both arguments per invocation and retains
neither, so nothing outside the page can re-run a section's fetch. Add two
private fields recorded on every `load`, and a `reload()` that re-invokes them
with `force = true`.

`reload()` is a no-op when `load` has never run — an instance constructed but
not yet loaded has nothing to repeat, and inventing a key would be worse than
doing nothing.

### The registry is tagged by route, never cleared on navigation

A module-level registry maps each live `CachedView` to **the route it last
loaded under**. `refreshAll()` calls `reload()` only on entries whose tag equals
the current route.

"Route" here is `$location` from svelte-spa-router — the path **without** the
query string. That is the right granularity and not an oversight: a page's
filters, search text and date range already live inside the `CachedView`'s own
`key`, which `reload()` replays verbatim. Tagging on the full URL would give one
page several tags and refresh only the sections loaded under the exact filter
combination showing at that instant, which is the opposite of "all sections".

Tagging rather than clearing-on-navigation is deliberate and is the one
non-obvious choice here. A `$effect` that clears the registry when `$location`
changes races the incoming page's own `load` calls: if the page registers first
and the clear runs second, the registry is empty and Refresh silently does
nothing on that page. The failure is invisible — no error, no console warning,
every test green — and it would appear on some pages and not others depending on
effect ordering. A tag comparison has no ordering to get wrong.

Entries accumulate for the life of the tab, bounded by the number of
`CachedView` instances ever constructed (~95). They are not payloads, so the
memory is negligible; `viewCache`'s `MAX_ENTRIES` cap is what bounds the data.

### The global button

A `RefreshButton` in `Topbar.svelte`, beside the theme toggle. On click:

1. Set the module-level `forcing` flag.
2. `viewCache.clear()` — **before** the reload, not after. Clearing afterwards
   would wipe the entries the reload just populated, so the next navigation
   back to this page would re-fetch data it had just been given.
3. `await refreshAll()`.
4. Poll any section still reporting not-fresh (below).
5. Clear the `forcing` flag.

The button is disabled while a refresh is in flight. It is **not** gated on any
permission: a user who cannot read a section simply has no `CachedView`
registered for it.

### `force=true` reaches the wire through an interceptor

The fetcher is an opaque closure the page built, so a forced reload cannot pass
a flag down to it without changing `load`'s signature — which would touch all 34
pages and re-introduce the drift this approach exists to avoid.

Instead a third `api.interceptors.request` appends `force=true` when the
`forcing` flag is set **and** the URL matches an explicit list of cached
endpoints. Both conditions matter: the flag alone would attach the param to
unrelated requests that happen to fire during the window, and an unknown query
param is only reliably harmless on routes we have checked.

The match is on the URL list alone, **not** on the HTTP method. An earlier
draft of this design said "GET only", which is wrong: `/v1/apps/{id}/funnel` is
a POST purely because its query is too big for a query string, and a method
rule would silently skip the one cached endpoint most likely to be expensive.
A query parameter rides a POST perfectly well — `Query<ForceQuery>` does not
care about the method — so the list is the authority and the method is a red
herring.

This is the one implicit mechanism in the design. It is confined to a single
function, mirrors `computeScopeParams` directly above it, and is pinned by a
test asserting that a non-cached URL never receives the param.

### Polling until fresh

A forced read returns an `Envelope` whose `state` is `Fresh`, `Stale` or
`Computing` and whose `data` may be `null`. The promise settling therefore does
**not** mean fresh data arrived, which is what decision 2 requires the button to
wait for.

After `refreshAll()` settles, any section whose envelope reports `Stale` or
`Computing` is re-read on an interval until it reports `Fresh`, or until a
**30-second cap**. On the cap the button stops spinning and the section renders
whatever it has, including its `error` if the recompute failed. A cap is
mandatory: a permanently failing aggregate would otherwise spin forever.

Overview already receives its recomputed sections over `/overview/stream` (SSE)
and keeps doing so — the poll is the generic fallback for `active-users`,
`funnels` and `admin/storage`, which have no push channel.

Pages whose data is not envelope-shaped (the majority — plain list endpoints
with no server cache) are fresh the moment their promise settles and are never
polled.

## Server design

### Thread the existing `force` through the four remaining sites

Add `Query<ForceQuery>` to three handlers, passing it into
`view_cache::read`'s existing `force` parameter. No new types.

| Handler | Route |
|---|---|
| `active_users::active_users` | `GET /v1/projects/{project_id}/active-users` |
| `funnels::compute` | `POST /v1/apps/{app_id}/funnel` |
| `admin::storage` | `GET /v1/admin/storage` |

The fourth cached read site, `active_users::active_users_csv`, is deliberately
**excluded**. It is an export the user explicitly asks for, not a section
rendered on screen, so it falls outside decision 4's reach — and forcing a
recompute for a file someone is about to download is a different intent from
refreshing a view. It keeps passing `false`.

Together with the five Overview section GETs that already accept `ForceQuery`,
that gives the client eight forceable endpoints.

### Cooldown

Before honouring a force, spend one unit of a Redis budget keyed on the app and
the resolved scope:

```
sauron:cache:force:{app_id}:{env_token}
```

`FORCE_COOLDOWN_SECS = 30`, limit 1 — one honoured force per app-and-scope per
30 seconds, shared across all users.

Uses `within_budget`, **not** `rate_limit`: an exhausted budget must
**downgrade the force to a normal read**, never return 429. The recompute the
budget was spent on is already running, so the correct answer is the ordinary
cached envelope — and a 429 would break a page render over a control the user
pressed hopefully. The response still says `Stale`/`Computing`, so the client
keeps polling and gets the fresh value when the in-flight recompute lands.

`within_budget` degrades to a per-process fallback when Redis is slow, which is
the right direction here: a degraded cooldown allows more forces per replica,
and `view_cache`'s single-flight claim plus its concurrency semaphore are still
in front of the aggregate.

This cooldown is applied to the five existing Overview section GETs as well,
closing the gap described above.

### Clearing the failure marker

`view_cache` writes a failure marker that suppresses re-enqueue **even under
`force`** (`view_cache.rs:233`). Without a change, a user clicking Refresh on a
section whose recompute recently failed gets no attempt at all, and — with the
spin-until-fresh behaviour — a spinner that runs to the 30s cap.

An **honoured** force therefore deletes `fail_key(key)` before the read. Only an
honoured one: a force downgraded by the cooldown must not clear it, or the
cooldown would stop bounding retries of a broken aggregate, which is the thing
the marker exists to prevent.

The marker keeps its full effect for automatic background revalidation, which is
its actual purpose. A person asking for data is a different question from a
poll asking on its own initiative.

## Testing

**Backend** (`bins/sauron-api/tests/`):

1. `force=true` on each of the four newly-threaded endpoints reaches
   `view_cache::read`'s force path — asserted by observing a recompute, not by
   reading the source.
2. The cooldown: two forces inside the window produce **one** recompute, and the
   second returns 200 with a cached envelope rather than 429.
3. An honoured force clears the failure marker; a **downgraded** force does not.
   These are separate tests — the second is the one that keeps the cooldown
   meaningful and is easy to lose in a refactor.
4. `force` does not appear in any cache key. A regression here is silent, so the
   test asserts that a forced and an unforced read of the same selection hit the
   same Redis key.

**Frontend** (`vitest`):

5. `reload()` re-invokes the last `(key, fetcher)` with `force = true`, and is a
   no-op before any `load`.
6. `refreshAll()` calls only instances tagged with the current route.
7. The registry survives a navigation away and back — the regression the
   clear-on-navigation design would have introduced.
8. The interceptor appends `force=true` only while `forcing` is set and only
   for a URL on the cached list — including the POST one, which a method-based
   rule would have skipped. A non-cached URL never receives it.
9. `viewCache.clear()` runs before the reload, not after.
10. The poll stops at the cap, and stops immediately once every section reports
    `Fresh`.

**Runtime drive.** Static gates cannot see a button that spins forever or one
that refreshes nothing. Drive a page with a server-cached section and confirm
`computed_at` actually advances; drive one without and confirm the button
settles promptly rather than polling for 30s.

## Out of scope

Per-page refresh buttons (the 21 existing ones stay as they are and keep
working); persisting the client cache; any change to `DEFAULT_FRESH_MS` or to
the per-route `CachePolicy` values; a push channel for the non-Overview cached
endpoints.
