/**
 * Loading a lazily-imported route chunk — with the FAILURE path spelled out.
 *
 * Every page in `routes.ts` is a `() => import('./pages/X.svelte')`. That is a
 * network request, and a network request fails: a deploy lands mid-session and
 * the browser still holds an `index.html` naming chunk hashes that no longer
 * exist; a CDN edge misses; a flaky connection drops one response. Before the
 * code split a boot either worked or failed visibly. After it, an unhandled
 * rejection leaves the router's `loadingComponent` on screen FOREVER — a
 * spinner with no error, no retry and no recovery short of a manual reload,
 * which is a worse failure than the bundle size the split removed.
 *
 * So the load is modelled as an outcome rather than a promise that may reject,
 * and the outcome is rendered (see `LazyRoute.svelte` / `RouteError.svelte`).
 *
 * ## There is deliberately NO retry, and that is a measured decision
 *
 * An earlier version of this file ran one bounded auto-retry (`attempts: 2`)
 * with a 350 ms pause, justified as "the commonest cause is transient". Counting
 * requests at the server disproved the justification outright:
 *
 *   * permanent 404, one fresh document load — the chunk was requested EXACTLY
 *     ONCE despite two attempts;
 *   * chunk restored on the server between the two attempts — still failed, and
 *     still exactly one request;
 *   * a manual "Try again" button afterwards — ZERO further requests.
 *
 * The module map is why. Per the ES module spec a specifier that failed is
 * recorded AS FAILED, so re-`import()`ing it in the same document replays the
 * cached rejection without touching the network. A second attempt cannot
 * succeed no matter what the server does. The retry was not insurance against a
 * transient blip; it was a no-op that also spent a third of a second of extra
 * spinner before every error state.
 *
 * Recovery is a document reload — verified working — which is why
 * `RouteError.svelte` offers exactly that and nothing else. Cache-busting the
 * specifier is NOT an alternative: appending a query string defeats Vite's
 * static analysis of the import graph, which is what produces the code split in
 * the first place.
 */

/** What `() => import('./pages/X.svelte')` resolves to. */
export type RouteChunkLoader<T> = () => Promise<T>;

export type RouteChunkOutcome<T> =
  | { status: 'loaded'; module: T }
  | { status: 'failed'; message: string };

/**
 * A human-readable reason, never an empty string.
 *
 * Vite's own message ("Failed to fetch dynamically imported module: …/assets/
 * Login-a1b2c3.js") already names the chunk, which is the single most useful
 * fact when the cause is a stale deploy — so it is surfaced verbatim rather
 * than replaced with generic copy.
 */
export function chunkErrorMessage(err: unknown): string {
  if (err instanceof Error && err.message.trim() !== '') return err.message;
  if (typeof err === 'string' && err.trim() !== '') return err;
  return 'The page could not be downloaded.';
}

/**
 * Run a chunk loader to an outcome. Never rejects — that is the whole point:
 * the caller renders what it gets back instead of hanging on a promise nobody
 * observes.
 *
 * The loader is invoked exactly once. See the header for the measurement that
 * removed the second invocation.
 */
export async function loadRouteChunk<T>(
  loader: RouteChunkLoader<T>,
): Promise<RouteChunkOutcome<T>> {
  try {
    // `loader()` is inside the `try` so a SYNCHRONOUS throw is an outcome too,
    // not a rejection escaping to the caller.
    return { status: 'loaded', module: await loader() };
  } catch (err) {
    return { status: 'failed', message: chunkErrorMessage(err) };
  }
}
