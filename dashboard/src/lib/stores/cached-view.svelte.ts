import { errorMessage } from '../api/client';
import { DEFAULT_FRESH_MS, viewCache } from './view-cache';

/**
 * One page's view of one cached payload: the data plus the three flags a
 * stale-while-revalidate render needs. Holds no fetching policy of its own —
 * `view-cache.ts` owns the cache semantics; this owns the reactive state and the
 * ordering guarantees.
 *
 * ## `data` holds a SHARED reference — treat it as immutable
 *
 * `data` is the very object the cache is holding, not a copy, so any in-place
 * write through it (`view.data[0].status = 'resolved'`) corrupts the cached
 * payload for every later reader, silently, for the life of the tab. Replace
 * `data` wholesale; never mutate into it. The `hands back the exact cached
 * reference` test pins this down.
 *
 * `$state.raw` rather than `$state` because the value is a plain container that
 * is always replaced, never edited in place: raw skips proxy setup and keeps
 * reference identity exact. Note it is NOT a safety mechanism — measured in this
 * project, `$state` at module scope did not proxy the assigned array either, and
 * even where it does proxy, writes go through to the same target. Nothing about
 * either choice prevents the mutation above; only the convention does.
 *
 * ## The three flags
 *
 * - `loading`      — nothing to show. Render the skeleton.
 * - `revalidating` — `data` is on screen and a refresh is in flight. Render the
 *                    data plus a subtle indicator; never a skeleton.
 * - `error`        — set ONLY when a failure left nothing to show. A failed
 *                    refresh over good data keeps the data and leaves this null,
 *                    because blanking a populated table over one bad poll is
 *                    worse than showing data a minute old.
 *
 * ## Out-of-order responses
 *
 * Filter, search and scope changes fire a new `load` before the previous one
 * settles, and nothing about HTTP guarantees they return in order. Each call
 * claims a generation and only the newest may write, so a slow response for
 * inputs the user has already moved off can never overwrite the current ones.
 */
export class CachedView<T> {
  data = $state.raw<T | undefined>(undefined);
  loading = $state(true);
  revalidating = $state(false);
  error = $state<string | null>(null);

  #gen = 0;
  #freshMs: number;

  constructor(freshMs: number = DEFAULT_FRESH_MS) {
    this.#freshMs = freshMs;
  }

  /** True once a payload has ever been shown — useful for "empty vs not yet loaded". */
  get hasData(): boolean {
    return this.data !== undefined;
  }

  /**
   * Paint whatever is cached for `key`, then refresh behind it if stale.
   *
   * `force` skips the fresh-window short-circuit: an explicit Refresh click or a
   * Retry after an error means "go to the network now", and honouring the cache
   * there makes the control look broken.
   *
   * Callers pass `key` and `fetcher` per invocation rather than at construction
   * because both depend on the page's current inputs, which are reactive.
   */
  async load(key: string, fetcher: () => Promise<T>, force = false): Promise<void> {
    const gen = ++this.#gen;
    // `!== undefined` and not a truthiness test: `undefined` is the cache's only
    // "absent" signal, so a payload that is legitimately null, 0, '' or [] counts
    // as a hit. A truthy check would send those down the skeleton path despite
    // being cached.
    const cached = viewCache.get<T>(key);
    if (cached !== undefined) {
      this.data = cached;
      this.error = null;
      this.loading = false;
    } else {
      // No hit: drop whatever the PREVIOUS key produced. Retaining it would
      // leave one environment's (or one filter's) rows sitting under the new
      // inputs, and any template that renders `data` alongside a loading
      // indicator rather than instead of it would present them as the new
      // result. `loading` alone is not enough to prevent that.
      this.loading = true;
      this.data = undefined;
      this.error = null;
    }
    if (!force && viewCache.isFresh(key, this.#freshMs)) return;
    this.revalidating = cached !== undefined;
    try {
      const fresh = await viewCache.dedupe(key, fetcher);
      if (gen !== this.#gen) return;
      // No `error = null` here: both branches above already cleared it before the
      // fetch started, and the generation guard means no other load can have set
      // it since. Kept out deliberately rather than left in as defensive noise —
      // an assignment no test can falsify is a line nobody can safely change.
      this.data = viewCache.set(key, fresh);
    } catch (err) {
      if (gen !== this.#gen) return;
      // Not cached on the failure path on purpose: caching it would make the
      // error sticky for the whole fresh window instead of retrying.
      if (cached === undefined) {
        this.error = errorMessage(err);
        this.data = undefined;
      }
    } finally {
      if (gen === this.#gen) {
        this.loading = false;
        this.revalidating = false;
      }
    }
  }

  /**
   * Abandon any in-flight load and reset to the pre-load state. For a page whose
   * inputs became unloadable (no app selected), so a late response from the
   * previous inputs cannot land afterwards.
   */
  reset(): void {
    this.#gen++;
    this.data = undefined;
    this.loading = true;
    this.revalidating = false;
    this.error = null;
  }
}
