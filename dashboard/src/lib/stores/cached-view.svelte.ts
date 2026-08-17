import { errorMessage, isNormalizedError } from '../api/client';
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
  /**
   * HTTP status behind `error`, or `null` when there is no error (or it never
   * reached the server).
   *
   * Kept beside the message because some failures are only actionable if the
   * page can tell them apart, and a formatted string cannot be interrogated
   * without matching on prose. The case that forced it: a 403 raised by a
   * permission-gated *filter* is permanent, so the generic Retry button offers a
   * recovery that provably cannot work, while "remove the filter" would. Every
   * page that only ever renders `error` is unaffected.
   */
  errorStatus = $state<number | null>(null);

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
   * `error` and `errorStatus` are set and cleared only through these two, and
   * that is the point: they are two halves of one fact, assigned at six separate
   * sites. Left as bare field writes, the next site added would set the message
   * and forget the status, and the symptom would be a stale 403 from a previous
   * failure still driving the recovery UI of the current one.
   */
  #setError(err: unknown): void {
    this.error = errorMessage(err);
    // `isNetwork` errors carry status 0 from `normalizeError`; normalized to
    // null so `errorStatus` means "the server answered with this" and nothing
    // else has to know 0 is a sentinel.
    this.errorStatus = isNormalizedError(err) && err.status > 0 ? err.status : null;
  }

  #clearError(): void {
    this.error = null;
    this.errorStatus = null;
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
      this.#clearError();
      this.loading = false;
    } else {
      // No hit: drop whatever the PREVIOUS key produced. Retaining it would
      // leave one environment's (or one filter's) rows sitting under the new
      // inputs, and any template that renders `data` alongside a loading
      // indicator rather than instead of it would present them as the new
      // result. `loading` alone is not enough to prevent that.
      this.loading = true;
      this.data = undefined;
      this.#clearError();
    }
    if (!force && viewCache.isFresh(key, this.#freshMs)) return;
    this.revalidating = cached !== undefined;
    try {
      // `fresh: force` — a forced fetch must not join a flight that started
      // before whatever prompted it. See `ViewCache.dedupe`.
      const fresh = await viewCache.dedupe(key, fetcher, force);
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
        this.#setError(err);
        this.data = undefined;
      } else if (force) {
        // Keep the stale data, but SAY SO. A background revalidate may fail
        // quietly — the user did not ask for it and the screen is still
        // truthful. An explicit Refresh or Retry is different: the user asked
        // for current data and did not get it. Swallowing that leaves stale
        // rows presented as fresh, with a spinner that stops and nothing else
        // changing, which reads as success. Reviewers found this same silent
        // failure on eight separate pages, which is what makes it the
        // primitive's bug rather than each page's.
        this.#setError(err);
      }
    } finally {
      if (gen === this.#gen) {
        this.loading = false;
        this.revalidating = false;
      }
    }
  }

  /**
   * Accept a value that arrived WITHOUT a fetch, and cache it under `key`.
   *
   * For server-pushed data: the Overview sections are recomputed in the
   * background and delivered over SSE, so the payload arrives with no request
   * to attach it to. Writing it through `viewCache.set` rather than only into
   * `data` is what makes it survive navigation — otherwise leaving the page and
   * coming back would re-show the pre-push value and re-request it, and the
   * push would look like it had never happened.
   *
   * Does NOT touch `loading`/`revalidating` beyond clearing `loading`: a push
   * can land while a fetch for the same key is in flight, and that fetch's own
   * `finally` still owns those flags.
   *
   * Ignores stale generations by design — it takes no generation guard, because
   * a push is keyed data, not a response to a specific load. The `key` is the
   * correctness boundary: a push for a key the page is no longer showing writes
   * to the cache under that key and never reaches `data`.
   */
  adopt(key: string, currentKey: string, value: T): void {
    viewCache.set(key, value);
    if (key !== currentKey) return;
    this.data = value;
    this.loading = false;
    this.#clearError();
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
    this.#clearError();
  }

  /**
   * Abandon any in-flight load and settle into "there is nothing to load".
   *
   * For a page that renders before its inputs exist — no app or project picked
   * yet. `loading` starts `true` and only a completed load clears it, so such a
   * page spins forever on a request that was never issued. Three separate
   * conversions hand-rolled a workaround for this (`!!projectId &&
   * view.loading`, `hasSelection && view.loading`), each subtly different and
   * each having to re-derive the page's own guard a second time; one of them
   * turned the empty state into a confident "No monitors yet" while the
   * selection was merely absent.
   *
   * Distinct from `reset()`, which means "start over, a load is coming".
   */
  idle(): void {
    this.#gen++;
    this.data = undefined;
    this.loading = false;
    this.revalidating = false;
    this.#clearError();
  }
}
