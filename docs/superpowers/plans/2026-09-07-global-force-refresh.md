# Global Force Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One refresh button in the shell that force-reloads every section on the current page and makes the server recompute, instead of re-serving its Redis cache.

**Architecture:** `CachedView` remembers the last `(key, fetcher)` it was called with and registers itself in a route-tagged registry, so a shell-level button can reload exactly the sections the current page loaded — with no change to any of the 34 pages. A request interceptor appends `force=true` for the eight cached endpoints while a forced refresh is in flight; the server threads that into the `force` parameter `view_cache::read` already accepts, behind a shared per-app cooldown.

**Tech Stack:** Svelte 5 (runes), axios, Rust (axum 0.8, Redis via `sauron-redis`).

**Spec:** `docs/superpowers/specs/2026-09-07-global-force-refresh-design.md` — read it first; this plan argues from it.

## Global Constraints

- **NEVER run `git commit` or create a branch.** Leave every change unstaged. This overrides the commit step in the task template below. Report what changed; the user commits.
- **`force` must NEVER reach a cache key.** A forced and an unforced read of the same selection are the same question and must share an entry. Keying on it gives Refresh a permanently-cold cache — a bug whose only symptom is that refreshing is always slow, which reads as correct behaviour. See `ForceQuery`'s doc at `routes/analytics.rs:947`.
- **The force must never put an aggregate on the request path.** It enqueues and returns; the client polls. This is the property `view_cache.rs` exists to preserve — abandoning it restores the 30s `TimeoutLayer` 503s.
- **An exhausted cooldown downgrades the force to a normal read — never a 429.** Use `within_budget`, not `rate_limit`.
- **`FORCE_COOLDOWN_SECS = 30`, limit 1**, keyed `sauron:cache:force:{app_id}:{env_token}`.
- **Poll cap is 30 seconds.** A permanently failing aggregate must not spin forever.
- **Every new dashboard string needs an Arabic translation** in the same catalog entry (`{ en: '…', ar: '…' }`).
- **The interceptor matches on the URL list, never on HTTP method** — `/v1/apps/{id}/funnel` is a cached POST.
- **Backend tests print `ok` having run nothing** without `TEST_DATABASE_URL`; the limiter assertions skip silently without `TEST_REDIS_URL`. Wall-clock duration is the only proof a suite ran.

### The eight forceable endpoints

Five already accept `ForceQuery`; three are added in Task 5.

| Route | Handler | Status |
|---|---|---|
| `GET /v1/apps/{app_id}/overview/totals` | `analytics::overview_totals` | already |
| `GET /v1/apps/{app_id}/overview/series` | `analytics::overview_series` | already |
| `GET /v1/apps/{app_id}/overview/top-issues` | `analytics::overview_top_issues` | already |
| `GET /v1/apps/{app_id}/overview/top-events` | `analytics::overview_top_events` | already |
| `GET /v1/apps/{app_id}/analytics/active-users` | `analytics::active_users_series` | already |
| `GET /v1/projects/{project_id}/active-users` | `active_users::active_users` | **add** |
| `POST /v1/apps/{app_id}/funnel` | `funnels::compute` | **add** |
| `GET /v1/admin/storage` | `admin::storage` | **add** |

`active_users::active_users_csv` is deliberately excluded — an export, not a rendered section.

---

### Task 1: `CachedView` remembers its last call

**Files:**
- Modify: `dashboard/src/lib/stores/cached-view.svelte.ts`
- Test: `dashboard/src/lib/stores/cached-view.test.ts` (extend if present, else create)

**Interfaces:**
- Consumes: nothing.
- Produces: `CachedView.reload(): Promise<void>` — re-invokes the last `(key, fetcher)` with `force = true`; resolves immediately as a no-op if `load` has never run. `CachedView.hasLoaded: boolean`. `CachedView.pending: boolean` — true only when the payload is a server-cache `Envelope` still reporting `stale`/`computing`.

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, it, vi } from 'vitest';
import { CachedView } from './cached-view.svelte';
import { viewCache } from './view-cache';

describe('CachedView.reload', () => {
  it('is a no-op before any load', async () => {
    const view = new CachedView<number>();
    // Inventing a key here would be worse than doing nothing: it would fetch
    // under a key no page ever reads.
    await expect(view.reload()).resolves.toBeUndefined();
    expect(view.hasLoaded).toBe(false);
  });

  it('re-invokes the last key and fetcher with force', async () => {
    viewCache.clear();
    const fetcher = vi.fn().mockResolvedValue(1);
    const view = new CachedView<number>();
    await view.load('k1', fetcher);
    expect(fetcher).toHaveBeenCalledTimes(1);

    // Without force this would short-circuit on the fresh window and never
    // reach the network — which is exactly the bug that makes a Refresh
    // button look broken.
    await view.reload();
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('is pending only for an envelope that is not yet fresh', async () => {
    viewCache.clear();
    const view = new CachedView<unknown>();

    // Ordinary list payloads are never pending: their promise settling IS
    // freshness, and treating them otherwise makes the global Refresh poll for
    // 30s on pages with no server cache at all.
    await view.load('plain', async () => [1, 2, 3]);
    expect(view.pending).toBe(false);

    await view.load('fresh', async () => ({ state: 'fresh', data: 1 }));
    expect(view.pending).toBe(false);

    await view.load('computing', async () => ({ state: 'computing', data: null }));
    expect(view.pending).toBe(true);

    await view.load('stale', async () => ({ state: 'stale', data: 1 }));
    expect(view.pending).toBe(true);
  });

  it('replays the MOST RECENT key, not the first', async () => {
    viewCache.clear();
    const f1 = vi.fn().mockResolvedValue(1);
    const f2 = vi.fn().mockResolvedValue(2);
    const view = new CachedView<number>();
    await view.load('k1', f1);
    await view.load('k2', f2);
    await view.reload();
    // A page whose filters moved must refresh what is on screen now, not what
    // was on screen when it mounted.
    expect(f2).toHaveBeenCalledTimes(2);
    expect(f1).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/stores/cached-view.test.ts`
Expected: FAIL — `view.reload is not a function`.

- [ ] **Step 3: Implement**

Add to the class body, beside the other private fields:

```ts
  /**
   * The last `(key, fetcher)` `load` was called with.
   *
   * Retained so something OUTSIDE the page — the global Refresh in the
   * Topbar — can repeat a section's fetch. `load` takes both per invocation
   * because they depend on the page's reactive inputs, so without this the
   * instance is the only thing that knows how to fetch itself and the only
   * thing that cannot be asked to.
   */
  #lastKey: string | null = null;
  #lastFetcher: (() => Promise<T>) | null = null;

  /** True once `load` has run at least once. */
  get hasLoaded(): boolean {
    return this.#lastKey !== null;
  }

  /**
   * Repeat the most recent `load` with `force = true`.
   *
   * A no-op before any `load`: an instance constructed but never loaded has
   * nothing to repeat, and inventing a key would fetch under one no page reads.
   */
  async reload(): Promise<void> {
    if (this.#lastKey === null || this.#lastFetcher === null) return;
    await this.load(this.#lastKey, this.#lastFetcher, true);
  }

  /**
   * True when this section's payload is a server-cache `Envelope` that has not
   * finished recomputing (`state` of `stale` or `computing`).
   *
   * Duck-typed on purpose. Only 8 of the ~95 `CachedView` instances hold an
   * envelope, and threading a type parameter through the other 87 to express
   * that would touch every page — the cost this whole approach exists to
   * avoid. Anything that is not an object carrying one of those two literal
   * states is simply not pending, so an ordinary list payload can never make
   * the global Refresh poll.
   */
  get pending(): boolean {
    const d = this.data as { state?: unknown } | undefined | null;
    if (!d || typeof d !== 'object') return false;
    return d.state === 'stale' || d.state === 'computing';
  }
```

And as the first two statements inside `load`, before `const gen = ++this.#gen;`:

```ts
    this.#lastKey = key;
    this.#lastFetcher = fetcher;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd dashboard && npx vitest run src/lib/stores/cached-view.test.ts`
Expected: PASS.

---

### Task 2: The route-tagged refresh registry

**Files:**
- Create: `dashboard/src/lib/stores/refresh-registry.ts`
- Create: `dashboard/src/lib/stores/refresh-registry.test.ts`
- Modify: `dashboard/src/lib/stores/cached-view.svelte.ts` (register on `load`)

**Interfaces:**
- Consumes: `CachedView.reload()` and `CachedView.hasLoaded` from Task 1.
- Produces:
  - `refreshRegistry.register(view: Refreshable, route: string): void`
  - `refreshRegistry.refreshAll(route: string): Promise<void>`
  - `refreshRegistry.pendingCount(route: string): number` — how many of that route's sections still report `stale`/`computing`; drives the poll loop in Task 7
  - `refreshRegistry.countFor(route: string): number` — test seam
  - `refreshRegistry.reset(): void` — test seam only; never called by app code
  - `interface Refreshable { reload(): Promise<void>; readonly pending: boolean }`

- [ ] **Step 1: Write the failing test**

```ts
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { refreshRegistry } from './refresh-registry';

function fake(pending = false) {
  return { reload: vi.fn().mockResolvedValue(undefined), pending };
}

beforeEach(() => refreshRegistry.reset());

describe('refreshRegistry', () => {
  it('refreshes only views tagged with the current route', async () => {
    const onIssues = fake();
    const onOverview = fake();
    refreshRegistry.register(onIssues, '/issues');
    refreshRegistry.register(onOverview, '/overview');

    await refreshRegistry.refreshAll('/issues');

    expect(onIssues.reload).toHaveBeenCalledTimes(1);
    // Refreshing a page you are not on would pay for aggregates nobody is
    // looking at — the reach the design explicitly rules out.
    expect(onOverview.reload).not.toHaveBeenCalled();
  });

  it('re-tags a view that loads again under a new route', async () => {
    const view = fake();
    refreshRegistry.register(view, '/issues');
    refreshRegistry.register(view, '/overview');

    await refreshRegistry.refreshAll('/issues');
    expect(view.reload).not.toHaveBeenCalled();

    await refreshRegistry.refreshAll('/overview');
    expect(view.reload).toHaveBeenCalledTimes(1);
  });

  it('registers each view once, however many times it loads', async () => {
    const view = fake();
    refreshRegistry.register(view, '/issues');
    refreshRegistry.register(view, '/issues');
    refreshRegistry.register(view, '/issues');

    expect(refreshRegistry.countFor('/issues')).toBe(1);
    await refreshRegistry.refreshAll('/issues');
    // Three loads (a filter changing twice) must not mean three parallel
    // refetches of the same section.
    expect(view.reload).toHaveBeenCalledTimes(1);
  });

  it('one failing view does not stop the others', async () => {
    const bad = { reload: vi.fn().mockRejectedValue(new Error('boom')) };
    const good = fake();
    refreshRegistry.register(bad, '/issues');
    refreshRegistry.register(good, '/issues');

    // `CachedView.reload` already records its own error state; the registry's
    // job is to make sure one broken section cannot leave the rest stale.
    await expect(refreshRegistry.refreshAll('/issues')).resolves.toBeUndefined();
    expect(good.reload).toHaveBeenCalledTimes(1);
  });

  it('counts only pending sections, and only for the current route', () => {
    refreshRegistry.register(fake(true), '/overview');
    refreshRegistry.register(fake(true), '/overview');
    refreshRegistry.register(fake(false), '/overview');
    refreshRegistry.register(fake(true), '/issues');

    // Drives the spinner: 0 means every section on THIS page is fresh and the
    // button must stop, regardless of what other routes are still computing.
    expect(refreshRegistry.pendingCount('/overview')).toBe(2);
    expect(refreshRegistry.pendingCount('/issues')).toBe(1);
    expect(refreshRegistry.pendingCount('/members')).toBe(0);
  });

  it('survives navigating away and back', async () => {
    const view = fake();
    refreshRegistry.register(view, '/issues');

    // Simulates: leave /issues, come back WITHOUT the section re-registering
    // (a cached hit can satisfy a load without a new registration in some
    // orderings). A clear-on-navigation design loses the entry here and
    // Refresh silently does nothing — no error, every test green.
    await refreshRegistry.refreshAll('/overview');
    await refreshRegistry.refreshAll('/issues');

    expect(view.reload).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/stores/refresh-registry.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement the registry**

```ts
/**
 * Which `CachedView`s belong to which route, so the global Refresh in the
 * Topbar can reload exactly the sections the current page has loaded.
 *
 * ## Why a route TAG and not a clear-on-navigation
 *
 * The obvious design — an `$effect` that empties the registry when `$location`
 * changes — races the incoming page's own `load` calls. If the page registers
 * first and the clear runs second, the registry is empty and Refresh silently
 * does nothing on that page: no error, no console warning, every test green,
 * and it would appear on some pages and not others depending on effect
 * ordering. A tag comparison has no ordering to get wrong.
 *
 * ## What "route" means
 *
 * `$location` from svelte-spa-router — the path WITHOUT the query string. A
 * page's filters, search text and date range already live inside the
 * `CachedView`'s own key, which `reload()` replays verbatim. Tagging on the
 * full URL would give one page several tags and refresh only the sections
 * loaded under the exact filter combination showing at that instant, which is
 * the opposite of "all sections".
 *
 * ## Growth
 *
 * Entries live for the tab's lifetime, bounded by the number of `CachedView`
 * instances ever constructed (~95 today). They hold no payloads — the data is
 * in `viewCache`, which has its own `MAX_ENTRIES` cap — so this is a few
 * kilobytes of references and deliberately has no eviction of its own.
 */
export interface Refreshable {
  reload(): Promise<void>;
  /** Still recomputing server-side. See `CachedView.pending`. */
  readonly pending: boolean;
}

class RefreshRegistry {
  /** view -> the route it last loaded under. A Map, so re-registering the
      same instance re-tags rather than duplicating it. */
  #routeOf = new Map<Refreshable, string>();

  register(view: Refreshable, route: string): void {
    this.#routeOf.set(view, route);
  }

  countFor(route: string): number {
    let n = 0;
    for (const tag of this.#routeOf.values()) if (tag === route) n += 1;
    return n;
  }

  /**
   * How many of `route`'s sections are still recomputing.
   *
   * The global Refresh spins until this reaches 0 (or its cap). Scoped to the
   * route so a slow aggregate on a page nobody is looking at cannot keep the
   * button spinning here.
   */
  pendingCount(route: string): number {
    let n = 0;
    for (const [view, tag] of this.#routeOf) if (tag === route && view.pending) n += 1;
    return n;
  }

  /**
   * Reload every view tagged with `route`, concurrently.
   *
   * `allSettled`, not `all`: each `CachedView.reload` already records its own
   * error into its own `error` field, and one broken section must not leave
   * every other section on the page stale.
   */
  async refreshAll(route: string): Promise<void> {
    const due: Refreshable[] = [];
    for (const [view, tag] of this.#routeOf) if (tag === route) due.push(view);
    await Promise.allSettled(due.map((v) => v.reload()));
  }

  /** Test seam. App code never calls this — see the clear-on-navigation note. */
  reset(): void {
    this.#routeOf.clear();
  }
}

export const refreshRegistry = new RefreshRegistry();
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd dashboard && npx vitest run src/lib/stores/refresh-registry.test.ts`
Expected: PASS.

The registry is deliberately standalone at this point — nothing registers into
it yet. `CachedView` is wired to it in Task 3, which is where the route source
it needs comes from.

---

### Task 3: Reading the current route without a store cycle

**Files:**
- Create: `dashboard/src/lib/stores/current-route.ts`
- Create: `dashboard/src/lib/stores/current-route.test.ts`

**Files (additional):**
- Modify: `dashboard/src/lib/stores/cached-view.svelte.ts` (register on `load` — Step 5)

**Interfaces:**
- Consumes: `refreshRegistry.register(view, route)` from Task 2.
- Produces: `currentRoute(): string`, `setCurrentRoute(path: string): void`, and the live wiring that puts every loaded `CachedView` into the registry.

Why a module rather than reading `$location` directly in `cached-view.svelte.ts`:
`cached-view` is imported by every page, and importing svelte-spa-router's
`location` store into it makes the primitive depend on the router. The same
bridge shape `configureAuthBridge` and `scope.ts` already use for the identical
cycle problem.

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, it } from 'vitest';
import { currentRoute, setCurrentRoute } from './current-route';

describe('currentRoute', () => {
  it("defaults to '' before the router reports anything", () => {
    // Registrations made before the first navigation are tagged '' and simply
    // never match a real route — inert, not wrong.
    expect(typeof currentRoute()).toBe('string');
  });

  it('round-trips what the router last reported', () => {
    setCurrentRoute('/issues');
    expect(currentRoute()).toBe('/issues');
  });

  it('strips a query string if one is passed', () => {
    // svelte-spa-router's `$location` already excludes it, but a caller
    // passing `window.location.hash` would otherwise create a second tag for
    // every filter combination and refresh almost nothing.
    setCurrentRoute('/issues?status=unresolved');
    expect(currentRoute()).toBe('/issues');
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/stores/current-route.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

```ts
/**
 * The route the app is currently on, as a plain module value.
 *
 * A bridge rather than a direct `$location` import, for the reason
 * `configureAuthBridge` and `scope.ts` document: `cached-view` is imported by
 * every page, and pulling the router into it creates a dependency the
 * primitive has no business having.
 */
let route = '';

export function currentRoute(): string {
  return route;
}

/**
 * Called from `App.svelte` whenever `$location` changes.
 *
 * Strips a query string defensively: `$location` never carries one, but a
 * future caller reaching for `window.location.hash` would otherwise mint a
 * separate tag per filter combination and make Refresh reload almost nothing.
 */
export function setCurrentRoute(path: string): void {
  route = path.split('?')[0];
}
```

- [ ] **Step 4: Wire it in `App.svelte`**

Beside the existing `$location`-driven effect:

```svelte
  // Feeds `refreshRegistry`'s tags. One writer, so nothing else has to import
  // the router.
  $effect(() => {
    setCurrentRoute($location);
  });
```

- [ ] **Step 5: Register from `CachedView.load`**

Now that both the registry (Task 2) and the route source exist, wire them into
the primitive. In `cached-view.svelte.ts`, beside the two assignments added in
Task 1:

```ts
    this.#lastKey = key;
    this.#lastFetcher = fetcher;
    // Registers on every load rather than at construction: construction has no
    // route yet, and re-registering is how a view that loads under a new route
    // gets re-tagged.
    refreshRegistry.register(this, currentRoute());
```

Add both imports at the top of the file:

```ts
import { refreshRegistry } from './refresh-registry';
import { currentRoute } from './current-route';
```

- [ ] **Step 6: Run to verify it passes**

Run: `cd dashboard && npx vitest run src/lib/stores/ && npm run check`
Expected: PASS, 0 svelte-check errors. This is the first point at which a real
`CachedView` appears in the registry, so a failure here is a wiring fault, not
a logic one.

---

### Task 4: The `force=true` request interceptor

**Files:**
- Modify: `dashboard/src/lib/api/scope.ts` (URL list + predicate)
- Modify: `dashboard/src/lib/api/client.ts` (interceptor)
- Create: `dashboard/src/lib/api/force.ts` (the flag)
- Modify: `dashboard/src/lib/api/scope.test.ts` (list assertions)
- Create: `dashboard/src/lib/api/force.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `FORCEABLE_URLS: readonly string[]` in `scope.ts`
  - `isForceableUrl(url: string | undefined): boolean` in `scope.ts`
  - `beginForcing(): void`, `endForcing(): void`, `isForcing(): boolean` in `force.ts`

- [ ] **Step 1: Write the failing test**

```ts
import { afterEach, describe, expect, it } from 'vitest';
import { beginForcing, endForcing, isForcing } from './force';
import { isForceableUrl } from './scope';

afterEach(() => endForcing());

describe('isForceableUrl', () => {
  it('matches every cached endpoint, including the POST one', () => {
    expect(isForceableUrl('/v1/apps/abc/overview/totals')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/overview/series')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/overview/top-issues')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/overview/top-events')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/analytics/active-users')).toBe(true);
    expect(isForceableUrl('/v1/projects/p1/active-users')).toBe(true);
    expect(isForceableUrl('/v1/admin/storage')).toBe(true);
    // POST only because its query is a body. A method-based rule would skip
    // the most expensive cached endpoint in the app.
    expect(isForceableUrl('/v1/apps/abc/funnel')).toBe(true);
  });

  it('does not match uncached endpoints', () => {
    expect(isForceableUrl('/v1/issues')).toBe(false);
    expect(isForceableUrl('/v1/orgs/o1/members')).toBe(false);
    // Neighbours of forceable routes, which a sloppy prefix match would catch.
    expect(isForceableUrl('/v1/apps/abc/overview')).toBe(false);
    expect(isForceableUrl('/v1/apps/abc/overview/stream')).toBe(false);
    expect(isForceableUrl('/v1/apps/abc/funnels')).toBe(false);
    expect(isForceableUrl(undefined)).toBe(false);
  });

  it('excludes the CSV export', () => {
    // An export the user explicitly asked for is not a section on screen.
    expect(isForceableUrl('/v1/projects/p1/active-users.csv')).toBe(false);
  });
});

describe('the forcing flag', () => {
  it('is off by default and toggles', () => {
    expect(isForcing()).toBe(false);
    beginForcing();
    expect(isForcing()).toBe(true);
    endForcing();
    expect(isForcing()).toBe(false);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/api/force.test.ts`
Expected: FAIL — modules not found.

- [ ] **Step 3: Implement the flag**

`dashboard/src/lib/api/force.ts`:

```ts
/**
 * Whether a user-initiated global Refresh is currently in flight.
 *
 * Read by the request interceptor in `client.ts` to append `force=true` to the
 * cached endpoints. A module flag rather than a parameter because the thing
 * that needs to say "force" (the Topbar button) and the thing that builds the
 * request (a closure a page constructed) are separated by `CachedView.load`,
 * whose signature is shared by all 34 pages — threading a flag through it
 * would touch every one of them and re-introduce exactly the drift the
 * registry design exists to avoid.
 *
 * Not reactive: nothing renders from it, and the window it covers is one
 * `await` inside a single click handler.
 */
let forcing = false;

export function beginForcing(): void {
  forcing = true;
}

export function endForcing(): void {
  forcing = false;
}

export function isForcing(): boolean {
  return forcing;
}
```

- [ ] **Step 4: Implement the URL predicate**

In `scope.ts`, beside the existing scope lists:

```ts
/**
 * The endpoints backed by the SERVER-side Redis result cache, and therefore
 * the only ones for which `force=true` means anything.
 *
 * Matched on the URL alone, never on HTTP method: `/v1/apps/{id}/funnel` is a
 * POST purely because its query is too large for a query string, and a method
 * rule would silently skip the most expensive cached endpoint here. A query
 * parameter rides a POST perfectly well.
 *
 * `/v1/projects/{id}/active-users.csv` is deliberately absent — it is an export
 * the user asked for, not a section on screen.
 *
 * Kept as a literal array because `scope.test.ts` asserts its contents against
 * the backend's own list of `ForceQuery` handlers; see the same cross-source
 * pattern `PROJECT_SCOPED_URLS` already uses.
 */
export const FORCEABLE_URLS: readonly RegExp[] = [
  /^\/v1\/apps\/[^/]+\/overview\/totals$/,
  /^\/v1\/apps\/[^/]+\/overview\/series$/,
  /^\/v1\/apps\/[^/]+\/overview\/top-issues$/,
  /^\/v1\/apps\/[^/]+\/overview\/top-events$/,
  /^\/v1\/apps\/[^/]+\/analytics\/active-users$/,
  /^\/v1\/projects\/[^/]+\/active-users$/,
  /^\/v1\/apps\/[^/]+\/funnel$/,
  /^\/v1\/admin\/storage$/,
];

export function isForceableUrl(url: string | undefined): boolean {
  if (!url) return false;
  const path = url.split('?')[0];
  return FORCEABLE_URLS.some((re) => re.test(path));
}
```

Anchored regexes, not `startsWith`: `/overview` and `/overview/stream` are
siblings of forceable routes and a prefix match would catch both — `stream` is
an SSE endpoint where an unknown query param is least welcome.

- [ ] **Step 5: Add the interceptor**

In `client.ts`, immediately after the `computeScopeParams` interceptor:

```ts
// ---------------------------------------------------------------------------
// Request interceptor — mark a user-initiated global Refresh.
//
// Both conditions are load-bearing. The flag alone would attach `force=true`
// to any unrelated request that happened to fire during the window; the URL
// list alone would attach it to every ordinary page load and defeat the cache
// entirely. See `./force.ts` for why this is a module flag and not a parameter.
// ---------------------------------------------------------------------------

api.interceptors.request.use((config: InternalAxiosRequestConfig) => {
  if (isForcing() && isForceableUrl(config.url)) {
    config.params = { ...(config.params as Record<string, unknown> | undefined), force: true };
  }
  return config;
});
```

- [ ] **Step 6: Run to verify it passes**

Run: `cd dashboard && npx vitest run src/lib/api/ && npm run check`
Expected: PASS, 0 svelte-check errors.

---

### Task 5: Thread `force` through the three remaining handlers

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/active_users.rs` (the `active_users` handler; **not** `active_users_csv`)
- Modify: `backend/bins/sauron-api/src/routes/funnels.rs` (`compute`)
- Modify: `backend/bins/sauron-api/src/routes/admin.rs` (`storage`)

**Interfaces:**
- Consumes: `routes::analytics::ForceQuery` (already `pub`), `view_cache::read(redis, key, policy, force)`.
- Produces: three handlers that honour `?force=true`.

- [ ] **Step 1: Add the extractor to each handler**

For each of the three, add a `Query(f): Query<ForceQuery>` extractor and replace
the hard-coded `false` with `f.force`. `ForceQuery` is already `pub` in
`routes/analytics.rs`; import it rather than defining a second copy — two
structs with the same wire name are how the two drift.

`active_users.rs:427`:

```rust
    let (envelope, _) =
        crate::view_cache::read(&state.redis, &inputs.key, &POLICY, f.force).await;
```

`funnels.rs:129` and `admin.rs:88` take the same change against their own
policy constants.

Extractor ordering: `Query<...>` is not body-consuming, so it may sit anywhere
before a `Json` body. In `funnels::compute` the `Json` extractor must stay
**last**.

- [ ] **Step 2: Add `ForceQuery` to each `#[utoipa::path]` params list**

Each handler's `params(...)` gains `ForceQuery`, so the OpenAPI document
describes the parameter. The route-parity test will not catch a missing one
(the route already exists), so this is a manual check.

- [ ] **Step 3: Verify it compiles and the doc builds**

Run: `cd backend && cargo check -p sauron-api && cargo test -p sauron-api --bin sauron-api openapi`
Expected: compiles; OpenAPI tests pass.

---

### Task 6: The cooldown and the failure-marker clear

**Files:**
- Modify: `backend/bins/sauron-api/src/view_cache.rs` (new `honour_force`)
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs` (5 overview GETs + `overview_refresh`)
- Modify: `backend/bins/sauron-api/src/routes/active_users.rs`, `funnels.rs`, `admin.rs`
- Test: `backend/bins/sauron-api/tests/http_force_refresh.rs` (create)

**Interfaces:**
- Consumes: `routes::auth::within_budget(state, key, limit, window) -> bool`, `view_cache::fail_key(key) -> String`.
- Produces: `view_cache::honour_force(state: &AppState, requested: bool, app_id: Uuid, env: &EnvFilter, key: &str) -> bool`.

- [ ] **Step 1: Implement `honour_force`**

```rust
/// One honoured force per app-and-scope per [`FORCE_COOLDOWN_SECS`], shared
/// across every user.
///
/// `within_budget`, NOT `rate_limit`: an exhausted budget must downgrade the
/// force to an ordinary cached read, never 429. The recompute the budget was
/// spent on is already running, so the correct answer is the cached envelope —
/// and a 429 would break a page render over a control the user pressed
/// hopefully. The response still reports `Stale`/`Computing`, so the client
/// keeps polling and receives the fresh value when that recompute lands.
///
/// Returns whether the force was honoured. An honoured force also clears the
/// failure marker, because `read` suppresses re-enqueue under a marker even
/// when forced — so without this, clicking Refresh on a recently-failed
/// section does nothing at all and the button spins to its cap. A DOWNGRADED
/// force must not clear it: that is what keeps the cooldown bounding retries of
/// a permanently broken aggregate.
pub async fn honour_force(
    state: &AppState,
    requested: bool,
    app_id: Uuid,
    env: &EnvFilter,
    key: &str,
) -> bool {
    if !requested {
        return false;
    }
    let budget_key = format!("sauron:cache:force:{}:{}", app_id, env_token(env));
    if !crate::routes::auth::within_budget(state, &budget_key, 1, FORCE_COOLDOWN_SECS).await {
        return false;
    }
    let _ = tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.del(&fail_key(key))).await;
    true
}

/// One honoured force per app-and-scope per this many seconds.
pub const FORCE_COOLDOWN_SECS: u64 = 30;
```

`RedisStore::del` already exists (`crates/sauron-redis/src/lib.rs:179`), and
`env_token` is already `pub` in this same module (`view_cache.rs:164`) — no new
helpers are needed.

- [ ] **Step 2: Route every force through it**

At each of the eight call sites, replace the raw force bool:

```rust
    let force = view_cache::honour_force(&state, f.force, app_id, env, &key).await;
    let (envelope, _) = crate::view_cache::read(&state.redis, &key, &POLICY, force).await;
```

`env` is the `&EnvFilter` each handler already holds to build its cache key —
the same value it passes to `overview_cache::cache_key` or its own key builder.
Do not re-derive it; a force keyed on a different scope than the entry it is
meant to invalidate would spend the wrong budget and clear the wrong marker.

`overview_refresh` (the admin POST) routes through it too, so the two paths
cannot disagree about what a force costs.

- [ ] **Step 3: Write the integration tests**

Create `backend/bins/sauron-api/tests/http_force_refresh.rs`, copying the
`TestServer` harness from `tests/http_password_reset.rs` verbatim (the suites
duplicate it deliberately). Cases:

1. `a_force_recomputes_and_advances_computed_at` — read a section, note
   `computed_at`, force, poll until `state == "fresh"`, assert `computed_at`
   moved.
2. `two_forces_inside_the_cooldown_produce_one_recompute` — force twice in
   quick succession; the second returns **200** with a cached envelope, never
   429, and `computed_at` advanced only once.
3. `an_honoured_force_clears_the_failure_marker` — write a failure marker
   directly, force, assert the marker is gone and a recompute was enqueued.
4. `a_downgraded_force_leaves_the_failure_marker` — spend the budget, write a
   marker, force again, assert the marker survives. This is the test that keeps
   the cooldown meaningful; it is the first thing a refactor loses.
5. `force_does_not_reach_the_cache_key` — a forced and an unforced read of the
   same selection resolve to the same Redis key. Assert on the key, not on
   timing: a key regression is otherwise silent and only shows up as "refresh
   is always slow", which reads as correct.
6. `the_csv_export_ignores_force` — `?force=true` on the CSV route does not
   spend the budget.

- [ ] **Step 4: Run**

Run: `cd backend && time cargo test -p sauron-api --test http_force_refresh`
Expected: 6 passed. **Check the elapsed time** — a sub-second run means
`TEST_DATABASE_URL` / `TEST_REDIS_URL` are unset and nothing executed.

---

### Task 7: The global button and the poll-until-fresh loop

**Files:**
- Create: `dashboard/src/lib/models/force-refresh.ts`
- Create: `dashboard/src/lib/models/force-refresh.test.ts`
- Modify: `dashboard/src/lib/components/layout/Topbar.svelte`
- Modify: `dashboard/src/lib/i18n/catalog/nav.ts`

**Interfaces:**
- Consumes: `refreshRegistry.refreshAll`, `currentRoute`, `beginForcing`/`endForcing`, `viewCache.clear`.
- Produces: `runGlobalRefresh(deps): Promise<void>`, `POLL_CAP_MS = 30_000`, `POLL_INTERVAL_MS = 1_500`.

The loop lives in a plain module, not in the component, so it is testable
without mounting the shell.

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, it, vi } from 'vitest';
import { runGlobalRefresh, POLL_CAP_MS } from './force-refresh';

function deps(over: Partial<Parameters<typeof runGlobalRefresh>[0]> = {}) {
  return {
    beginForcing: vi.fn(),
    endForcing: vi.fn(),
    clearCache: vi.fn(),
    refreshAll: vi.fn().mockResolvedValue(undefined),
    pendingCount: vi.fn().mockReturnValue(0),
    sleep: vi.fn().mockResolvedValue(undefined),
    now: (() => { let t = 0; return () => (t += 1_000); })(),
    ...over,
  };
}

describe('runGlobalRefresh', () => {
  it('clears the cache BEFORE reloading, not after', async () => {
    const order: string[] = [];
    const d = deps({
      clearCache: vi.fn(() => void order.push('clear')),
      refreshAll: vi.fn(async () => void order.push('refresh')),
    });
    await runGlobalRefresh(d);
    // Reversed, the clear would wipe the entries the reload just populated,
    // so navigating back to this page would refetch what it was just given.
    expect(order).toEqual(['clear', 'refresh']);
  });

  it('always clears the forcing flag, even when the reload throws', async () => {
    const d = deps({ refreshAll: vi.fn().mockRejectedValue(new Error('boom')) });
    await runGlobalRefresh(d);
    // A stuck flag would append force=true to every later request for the
    // life of the tab, defeating the cache entirely.
    expect(d.endForcing).toHaveBeenCalledTimes(1);
  });

  it('stops polling as soon as every section reports fresh', async () => {
    const pendingCount = vi.fn().mockReturnValueOnce(2).mockReturnValueOnce(1).mockReturnValue(0);
    const d = deps({ pendingCount });
    await runGlobalRefresh(d);
    expect(d.sleep).toHaveBeenCalledTimes(2);
  });

  it('gives up at the cap rather than spinning forever', async () => {
    // A permanently failing aggregate never reports fresh.
    const d = deps({ pendingCount: vi.fn().mockReturnValue(1) });
    await runGlobalRefresh(d);
    expect(d.sleep.mock.calls.length).toBeLessThanOrEqual(POLL_CAP_MS / 1_000);
  });

  it('does not poll at all when nothing is server-cached', async () => {
    // Most pages have no envelope-shaped section; their promise settling IS
    // freshness, and a 30s spinner there would be a bug.
    const d = deps({ pendingCount: vi.fn().mockReturnValue(0) });
    await runGlobalRefresh(d);
    expect(d.sleep).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/models/force-refresh.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

```ts
/** How long the button may keep polling before giving up. */
export const POLL_CAP_MS = 30_000;
/** Gap between freshness polls. */
export const POLL_INTERVAL_MS = 1_500;

export interface GlobalRefreshDeps {
  beginForcing(): void;
  endForcing(): void;
  clearCache(): void;
  refreshAll(): Promise<void>;
  /** How many on-screen sections still report Stale or Computing. */
  pendingCount(): number;
  sleep(ms: number): Promise<void>;
  now(): number;
}

/**
 * One click of the global Refresh.
 *
 * Dependency-injected rather than reaching for the stores directly so the
 * ordering guarantees below are testable without mounting the shell — they are
 * the whole substance of this function and each one is a real bug otherwise.
 */
export async function runGlobalRefresh(d: GlobalRefreshDeps): Promise<void> {
  d.beginForcing();
  try {
    // BEFORE the reload. Reversed, this wipes the entries the reload just
    // populated and the next navigation refetches data it was just given.
    d.clearCache();
    await d.refreshAll();

    // A forced read answers immediately with `Computing`/`Stale` and a possibly
    // null `data` — the recompute runs off the request path. So a settled
    // promise is not fresh data, and the button must keep spinning until the
    // sections say so.
    const deadline = d.now() + POLL_CAP_MS;
    while (d.pendingCount() > 0 && d.now() < deadline) {
      await d.sleep(POLL_INTERVAL_MS);
    }
  } finally {
    // `finally`, unconditionally: a flag left set appends force=true to every
    // later request for the life of the tab.
    d.endForcing();
  }
}
```

- [ ] **Step 4: Add the button to `Topbar.svelte`**

Beside the theme toggle, using the existing `.icon-btn` class so it matches its
neighbours:

```svelte
    <button
      class="icon-btn"
      title={t('nav.refreshAll')}
      aria-label={t('nav.refreshAll')}
      disabled={refreshing}
      onclick={doRefresh}
    >
      <span class:spinning={refreshing}><Icon name="refresh" size={16} /></span>
    </button>
```

with, in the script block:

```svelte
  let refreshing = $state(false);

  async function doRefresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      await runGlobalRefresh({
        beginForcing,
        endForcing,
        clearCache: () => viewCache.clear(),
        refreshAll: () => refreshRegistry.refreshAll(currentRoute()),
        pendingCount: () => refreshRegistry.pendingCount(currentRoute()),
        sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
        now: () => Date.now(),
      });
    } finally {
      refreshing = false;
    }
  }
```

`refreshRegistry.pendingCount` and `CachedView.pending` come from Tasks 2 and 1
respectively; nothing new is needed here. Non-envelope payloads are never
pending, which is what keeps ordinary pages from polling at all.

- [ ] **Step 5: i18n**

`catalog/nav.ts`:

```ts
  'nav.refreshAll': { en: 'Refresh all data', ar: 'تحديث كل البيانات' },
```

- [ ] **Step 6: Run**

Run: `cd dashboard && npm run check && npm test`
Expected: 0 errors; all suites pass.

- [ ] **Step 7: Drive it in a browser**

Static gates cannot see a button that spins forever or refreshes nothing. Boot
the API and dashboard, then:

1. On **Overview** (server-cached), note `computed_at`, click Refresh, confirm
   the spinner runs and `computed_at` advances.
2. On **Members** (no server cache, and one of the 13 pages with no refresh
   control today), click Refresh and confirm it settles promptly rather than
   polling for 30 seconds.
3. Click Refresh twice quickly and confirm the second is a 200, not a 429.
4. Confirm a request to an uncached endpoint made during a refresh does **not**
   carry `force=true` — check the network panel.

---

## Verification

```bash
cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets
cd backend && time cargo test -p sauron-api --test http_force_refresh && time cargo test --workspace
cd dashboard && npm run check && npm test
```

Report the wall-clock duration of the backend suites. A `cargo fmt` failure
short-circuits CI and skips clippy and test entirely, so confirm those actually
ran rather than reading a green summary.

**Do not commit.** Leave everything in the working tree.
