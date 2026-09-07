/**
 * The route the app is currently on, as a plain module value.
 *
 * A bridge rather than a direct `$location` import, for the reason
 * `configureAuthBridge` and `api/scope.ts` already document: `cached-view` is
 * imported by every page, and pulling svelte-spa-router into it would make the
 * caching primitive depend on the router — a dependency it has no business
 * having, and one more import cycle waiting to happen.
 *
 * Written from exactly one place (`App.svelte`'s `$location` effect) and read
 * by `CachedView.load` to tag its registration. Deliberately not reactive:
 * nothing renders from it, and its only reader runs inside `load`.
 */
let route = '';

/**
 * Bumped on every navigation, including a return to a route already visited.
 *
 * `CachedView` instances are constructed per MOUNT, not once per route, so
 * leaving and re-entering a page creates a second full set of them. Without a
 * visit counter the registry keeps both sets under the same route tag and
 * Refresh fires every request twice — three times on the third visit — while
 * the orphaned views' retained fetchers keep destroyed component scopes alive.
 * An earlier comment here claimed growth was "bounded by the number of
 * CachedView instances ever constructed"; that was wrong, and this is the fix.
 */
let visit = 0;

export function currentRoute(): string {
  return route;
}

/** Which visit to `currentRoute()` we are on. See [`visit`]. */
export function routeVisit(): number {
  return visit;
}

/**
 * Record the route the app has navigated to.
 *
 * Strips a query string defensively. `$location` never carries one, but a
 * future caller reaching for `window.location.hash` would otherwise mint a
 * separate tag per filter combination — and the global Refresh would then
 * reload only the sections loaded under the exact filters showing at that
 * instant, which is the opposite of what it promises.
 */
export function setCurrentRoute(path: string): void {
  const next = path.split('?')[0];
  // Bumped even when the path is unchanged is WRONG — Svelte re-runs this
  // effect on unrelated store changes, and a bump mid-visit would orphan the
  // views this page already registered. Only a real navigation counts.
  if (next === route) return;
  route = next;
  visit += 1;
}
