import { beginForcing, endForcing } from '../api/force';
import { runPageRefresh } from '../models/force-refresh';
import { currentRoute } from './current-route';
import { refreshRegistry } from './refresh-registry';

/**
 * A page's Refresh button, in one line.
 *
 * Every page's refresh does something slightly different — `Events` kicks a
 * rollup fold, `Overview` drives SSE, most just reload their lists — so this
 * takes the page's own body rather than replacing it. What it standardises is
 * everything around that body: the force window, the busy flag, the
 * double-click guard, and the wait for server-side recomputes to land.
 *
 * Before this existed each page hand-rolled a `refreshing` flag and a
 * try/finally, and none of them forced the server, so the page button and the
 * one in the top bar meant different things on the four server-cached pages.
 *
 * Usage:
 *
 * ```svelte
 * const refresher = pageRefresher(async () => {
 *   await Promise.all([load(appId, true), loadStats(appId, range, true)]);
 * });
 * <RefreshButton onclick={refresher.run} loading={refresher.busy} />
 * ```
 *
 * Pass `force = true` down to your own loaders inside the body: this opens the
 * SERVER-side force window, while bypassing the client's freshness window is
 * still each `CachedView.load` call's own third argument.
 */
export function pageRefresher(body: () => Promise<void>) {
  let busy = $state(false);

  return {
    get busy() {
      return busy;
    },
    async run(): Promise<void> {
      // Guard rather than queue: a second click during a refresh wants the
      // same answer the first one is already fetching, and running the body
      // twice would spend the cooldown for nothing.
      if (busy) return;
      busy = true;
      try {
        await runPageRefresh({
          beginForcing,
          endForcing,
          body,
          // Scoped to the current route, so a slow aggregate on a page nobody
          // is looking at cannot keep this button spinning.
          pendingCount: () => refreshRegistry.pendingCount(currentRoute()),
          sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
          now: () => Date.now(),
        });
      } finally {
        busy = false;
      }
    },
  };
}
