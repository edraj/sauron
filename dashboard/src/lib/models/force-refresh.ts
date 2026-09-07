/**
 * What one click of the global Refresh does.
 *
 * Lives in a plain module rather than inside `Topbar.svelte` because the whole
 * substance of it is a handful of ordering guarantees, each of which is a real
 * bug if it flips — and none of which are testable without mounting the shell
 * if they live in a component.
 */

/**
 * How long the button may keep polling before giving up.
 *
 * A cap is mandatory, not defensive: a permanently failing aggregate never
 * reports fresh, and without this the spinner runs for the life of the page.
 */
export const POLL_CAP_MS = 30_000;

/** Gap between freshness polls. */
export const POLL_INTERVAL_MS = 1_500;

export interface GlobalRefreshDeps {
  beginForcing(): void;
  endForcing(): void;
  /**
   * Drop every cached entry EXCEPT the keys just repopulated.
   *
   * Not a plain `clear()`, and not called before the reload. Clearing first
   * makes every `CachedView.load` miss, so the page blanks to skeletons for the
   * whole request and shows a hard error rather than stale data if it fails —
   * the opposite of the stale-while-revalidate behaviour the cache exists for.
   */
  clearCacheExcept(keep: ReadonlySet<string>): void;
  refreshAll(): Promise<string[]>;
  /** How many on-screen sections still report `stale` or `computing`. */
  pendingCount(): number;
  sleep(ms: number): Promise<void>;
  now(): number;
}

/**
 * Force-reload every section on the current page, then wait for the server to
 * finish recomputing the cached ones.
 *
 * Dependency-injected so the ordering below can be asserted directly. In order,
 * and each for its own reason:
 *
 * 1. `beginForcing` FIRST, so the reload's requests actually carry
 *    `force=true`. Set afterwards, every request would go out unforced, the
 *    server would answer from Redis, and the button would look like it worked
 *    while changing nothing.
 * 2. Reload, THEN drop the rest of the cache — never the other way round. An
 *    earlier version cleared first, which made every reload a cache miss and
 *    blanked the page to skeletons for the duration of the request. Keeping the
 *    keys just repopulated is what lets the other routes be invalidated without
 *    the current one flickering.
 * 3. `endForcing` in a `finally`, unconditionally — and before the poll. A flag
 *    left set appends `force=true` to every later request for the life of the
 *    tab; left set through the poll, every poll would re-force.
 * 4. Poll while anything is still recomputing, RE-READING each tick. A forced
 *    read answers immediately — the aggregate runs off the request path, and a
 *    still-fresh entry even comes back `state: "fresh"` with
 *    `recomputing: true` — so a settled promise is not fresh data. The polls
 *    are unforced, so they observe the recompute landing without starting
 *    another one.
 */
export interface PageRefreshDeps {
  beginForcing(): void;
  endForcing(): void;
  /**
   * The page's OWN refresh work.
   *
   * Deliberately not replaced by a registry sweep. Several pages do real
   * page-specific work here that a generic sweep would drop — `Events` kicks a
   * rollup fold before reloading so its aggregates include the newest events,
   * and `Overview` drives its SSE path. Unifying the buttons means running
   * these bodies inside the same force window, not replacing them.
   */
  body(): Promise<void>;
  /** How many of THIS page's sections still report `stale`/`computing`. */
  pendingCount(): number;
  sleep(ms: number): Promise<void>;
  now(): number;
}

/**
 * Poll until nothing on the page is recomputing, re-reading each tick.
 *
 * Shared by both entry points because the reasoning is identical and getting
 * it wrong is silent: `pendingCount()` reads the payload the last fetch
 * produced, so without a re-read it can never fall and the loop degrades into
 * a fixed-length sleep that always ends at the cap.
 *
 * The caller must have closed the force window before calling this. A forced
 * poll spends the cooldown, re-triggers the aggregate, and comes back
 * `recomputing: true` again — so the button would run to its cap every time.
 */
async function pollUntilSettled(
  d: Pick<PageRefreshDeps, 'pendingCount' | 'sleep' | 'now'>,
  reread: () => Promise<void>,
): Promise<void> {
  try {
    const deadline = d.now() + POLL_CAP_MS;
    while (d.pendingCount() > 0 && d.now() < deadline) {
      await d.sleep(POLL_INTERVAL_MS);
      await reread();
    }
  } catch {
    // Swallowed deliberately. Each section records its own error, and
    // rethrowing here would only give the click an unhandled rejection.
  }
}

/**
 * What one click of a PAGE's Refresh button does.
 *
 * The same force window and the same wait-until-fresh behaviour as the global
 * button, so "refresh" means one thing everywhere — but scoped to this page.
 *
 * The one deliberate difference: there is no `clearCache`. A page-level
 * refresh means "this page again", not "distrust everything"; dropping every
 * other view's entry would quietly turn each of the 34 page buttons into an
 * app-wide cache flush.
 */
export async function runPageRefresh(d: PageRefreshDeps): Promise<void> {
  d.beginForcing();
  try {
    await d.body();
  } catch {
    // The page's own body reports its own failures through its CachedViews.
  } finally {
    d.endForcing();
  }
  await pollUntilSettled(d, () => d.body());
}

export async function runGlobalRefresh(d: GlobalRefreshDeps): Promise<void> {
  d.beginForcing();
  try {
    const refreshed = await d.refreshAll();
    d.clearCacheExcept(new Set(refreshed));
  } catch {
    // See the note below; the forced pass owns its own per-section errors.
  } finally {
    // Cleared BEFORE the poll, not after, and this ordering is the whole point
    // of the loop below. The polls re-read the same sections to observe the
    // recompute landing — they must NOT carry `force=true` themselves, or every
    // poll would spend the cooldown, re-trigger the aggregate, and report
    // `recomputing: true` forever. The button would then always run to its cap.
    d.endForcing();
  }

  // The polls discard the returned keys: invalidation already happened above,
  // and a poll only needs to observe the recompute landing.
  await pollUntilSettled(d, async () => {
    await d.refreshAll();
  });
}
