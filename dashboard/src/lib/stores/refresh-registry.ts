import { routeVisit } from './current-route';

/**
 * Which `CachedView`s belong to the current visit to the current route, so the
 * global Refresh in the Topbar can reload exactly the sections on screen.
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
 * ## Growth, and why the visit counter exists
 *
 * `CachedView` instances are constructed per MOUNT, not once per route. Leaving
 * a page and coming back builds a second full set, and an earlier version of
 * this file kept both under the same route tag — so Refresh fired every request
 * twice on the second visit and three times on the third, while the orphaned
 * views' retained fetchers kept destroyed component scopes alive for the life
 * of the tab.
 *
 * Each entry therefore records the visit it was registered on, and `register`
 * drops everything from earlier visits. That bounds the map to the sections of
 * the page currently on screen, which is also exactly the set Refresh should
 * ever touch.
 */
export interface Refreshable {
  reload(): Promise<void>;
  /** Still recomputing server-side. See `CachedView.pending`. */
  readonly pending: boolean;
  /**
   * The cache key this view last populated, or `null` if it has none.
   *
   * Reported back by `refreshAll` so the global Refresh can drop every other
   * cache entry without dropping what it just repopulated. Adapters that drive
   * `viewCache` directly (Projects) return `null`: their entries are simply
   * dropped and refetched on the next visit.
   */
  readonly lastKey: string | null;
}

class RefreshRegistry {
  /**
   * view -> the route and visit it last loaded under.
   *
   * A `Map` keyed on the instance, so re-registering the same view re-tags it
   * rather than duplicating it: a page whose filters change calls `load` again
   * and must not end up refreshed N times in parallel.
   */
  #tagOf = new Map<Refreshable, { route: string; visit: number }>();

  register(view: Refreshable, route: string): void {
    const visit = routeVisit();
    // Evict everything from an earlier visit. Views from a previous mount of
    // this very route would otherwise stay registered under the same tag and
    // double every request — see the class doc.
    for (const [other, tag] of this.#tagOf) {
      if (tag.visit !== visit) this.#tagOf.delete(other);
    }
    this.#tagOf.set(view, { route, visit });
  }

  /** How many views are registered for `route` on this visit. Test seam. */
  countFor(route: string): number {
    let n = 0;
    const visit = routeVisit();
    for (const tag of this.#tagOf.values()) {
      if (tag.route === route && tag.visit === visit) n += 1;
    }
    return n;
  }

  /**
   * How many of `route`'s sections are still recomputing server-side.
   *
   * The global Refresh spins until this reaches 0 (or its cap). Read live on
   * every poll rather than snapshotted, because the whole point is to observe
   * it fall. Scoped to the route so a slow aggregate on a page nobody is
   * looking at cannot keep the button spinning here.
   */
  pendingCount(route: string): number {
    let n = 0;
    const visit = routeVisit();
    for (const [view, tag] of this.#tagOf) {
      if (tag.route === route && tag.visit === visit && view.pending) n += 1;
    }
    return n;
  }

  /**
   * Reload every view tagged with `route`, concurrently.
   *
   * `allSettled`, not `all`: each `CachedView.reload` already records its own
   * failure into its own `error` field, and one broken section must not leave
   * every other section on the page stale.
   *
   * Returns the cache keys it repopulated, so the caller can invalidate the
   * rest without blanking the page it just refreshed.
   */
  async refreshAll(route: string): Promise<string[]> {
    const visit = routeVisit();
    const due: Refreshable[] = [];
    for (const [view, tag] of this.#tagOf) {
      if (tag.route === route && tag.visit === visit) due.push(view);
    }
    await Promise.allSettled(due.map((v) => v.reload()));
    return due.map((v) => v.lastKey).filter((k): k is string => k !== null);
  }

  /**
   * Total retained entries, across every route and visit. Test seam.
   *
   * Exists to make the eviction in `register` falsifiable. The visit filter in
   * `refreshAll` already stops an old mount's views from FIRING, so a test that
   * only checks call counts passes with or without the eviction — this is the
   * one that fails without it.
   */
  size(): number {
    return this.#tagOf.size;
  }

  /** Test seam. App code never calls this — see the clear-on-navigation note. */
  reset(): void {
    this.#tagOf.clear();
  }
}

export const refreshRegistry = new RefreshRegistry();
