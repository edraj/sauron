/**
 * Per-view payload cache, so navigating back to a page you just left paints
 * instantly instead of blanking to a skeleton while the same request runs again.
 *
 * This is deliberately NOT a reactive store. Pages keep their own `$state` for
 * `data` / `loading` / `error` exactly as they do today; this module only
 * answers "do I already have rows for this key, and are they still fresh?".
 * That keeps the wiring per page down to a few lines and leaves every existing
 * template binding untouched.
 *
 * ## Three states, not two
 *
 * The bug this exists to fix is that a page has a single `loading` flag, so it
 * cannot distinguish "I have nothing" from "I have something slightly old".
 * With only one flag the honest render is a skeleton, which is why every
 * navigation flashes empty. Callers should now track:
 *
 * - `loading`      — nothing cached. Render the skeleton/spinner.
 * - `revalidating` — cached rows are on screen and a refresh is in flight.
 *                    Render the rows plus a subtle indicator (`RefreshButton`
 *                    already spins on its `loading` prop — pass this).
 * - neither        — fresh cached rows, no request made at all.
 *
 * ## Memory only, on purpose
 *
 * There is no `localStorage`/`sessionStorage` backing and none should be added.
 * These payloads are error bodies, breadcrumbs, user traits and session rows —
 * the same content the PII Inspector exists to find. Persisting them would turn
 * a session-scoped, RBAC-gated read into an at-rest copy sitting outside every
 * retention window and permission check the backend enforces, readable by
 * anything else running on that origin. A tab reload starting cold is the
 * correct trade.
 *
 * ## The cache key must carry the scope
 *
 * `sessionStore.scopeKey` is `appId:envId`, and telemetry pages already key
 * their effects on it because an effect tracking only the app "will not re-run
 * when the environment changes, leaving the previous environment's data on
 * screen" (session.svelte.ts). A cache key has the same requirement but fails
 * worse: a missing scope component does not merely leave stale rows up, it
 * serves one environment's rows *as* another's. Include every id the request
 * varies on — `viewKey()` exists to make that a one-liner.
 *
 * Entries are also dropped wholesale on logout (`authStore.clearLocal`), so a
 * second user signing in on the same tab can never be served the first user's
 * rows.
 *
 * ## Cached values are immutable
 *
 * `set()` stores the reference it is given and `get()` hands that same
 * reference back, so a caller that mutates a cached payload in place corrupts
 * every later read of that key. Worse, assigning a cached value into a `$state`
 * variable wraps it in Svelte's deep proxy, and writes through that proxy reach
 * the cached object — so the corruption is silent and survives navigation.
 *
 * Verified safe at the time of writing: no page mutates a fetched payload in
 * place. The two in-place `.sort()` calls in `src/pages` both sort a fresh
 * array, not the response — `PersonProfile.svelte`'s sorts a locally-built
 * `TimelineItem[]`, and `JourneyExplorer.svelte`'s sorts the array returned by
 * `.filter()`. Every other list-mutating call site already copies first
 * (`[...journey.links].sort(...)`, `checks.slice().sort(...)`). Keep it that
 * way: copy before sorting, and replace rather than splice.
 */

/**
 * How long an entry counts as fresh. Inside this window a revisit does not hit
 * the network at all; past it the cached rows still paint immediately and a
 * refresh runs behind them.
 */
export const DEFAULT_FRESH_MS = 60_000;

/**
 * Hard cap on retained entries, evicting least-recently-used first. Bounded
 * because keys include filter/search/date-range state: a user typing in a
 * search box walks through a new key per keystroke, so an unbounded map would
 * retain every intermediate result set for the life of the tab.
 */
export const MAX_ENTRIES = 200;

export interface CacheEntry<T> {
  data: T;
  /** `Date.now()` at the moment this payload was stored. */
  storedAt: number;
}

/**
 * Build a cache key from a view name plus whatever the request varies on.
 *
 * Each part is JSON-encoded and joined with NUL, which is what keeps
 * `viewKey('a', 'b')` from colliding with `viewKey('a|b')` — a plain `join('|')`
 * would map both to the same string, and the collision would surface as one
 * view serving another's payload. Object parts have their keys sorted so two
 * structurally-equal filter objects produce one key regardless of literal order.
 *
 * `undefined` survives as a distinct token rather than being dropped: a request
 * with no `q` and a request with `q=''` are different requests.
 */
export function viewKey(view: string, ...parts: unknown[]): string {
  return [view, ...parts.map(stableStringify)].join('\u0000');
}

function stableStringify(value: unknown): string {
  if (value === undefined) return 'undefined';
  return JSON.stringify(value, (_k, v: unknown) => {
    if (v === null || typeof v !== 'object' || Array.isArray(v)) return v;
    const src = v as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(src).sort()) out[k] = src[k];
    return out;
  });
}

class ViewCache {
  // Insertion order IS the LRU order: `touch()` re-inserts on every read and
  // write, so the oldest key is always the first one the iterator yields.
  private entries = new Map<string, CacheEntry<unknown>>();

  // Fetches currently in flight, keyed the same way. Two effects that resolve
  // to the same key (a page's own load plus a manual refresh, or two effects
  // both keyed on `scopeKey`) share one request instead of racing two.
  private inflight = new Map<string, Promise<unknown>>();

  /** Cached payload for `key`, or `undefined` if there is none. Counts as a use. */
  get<T>(key: string): T | undefined {
    const hit = this.entries.get(key);
    if (!hit) return undefined;
    this.touch(key, hit);
    return hit.data as T;
  }

  /**
   * Like `get`, but exposes `storedAt` and does NOT count as a use — for
   * rendering "updated 20s ago" without perturbing eviction order.
   */
  peek<T>(key: string): CacheEntry<T> | undefined {
    return this.entries.get(key) as CacheEntry<T> | undefined;
  }

  /**
   * True iff `key` holds a payload stored within `freshMs`. False for a missing
   * key, so `isFresh` alone can never be read as "there is data here" — callers
   * check `get()` for presence and `isFresh()` only to decide whether to skip
   * the network.
   */
  isFresh(key: string, freshMs: number = DEFAULT_FRESH_MS): boolean {
    const hit = this.entries.get(key);
    if (!hit) return false;
    return Date.now() - hit.storedAt < freshMs;
  }

  /**
   * Store `data` under `key` and return it unchanged, so a call site can wrap
   * an assignment directly: `rows = viewCache.set(key, await fetchRows())`.
   *
   * Only ever call this with a successful response. Caching a failure would
   * make the error sticky for the whole fresh window; a failed revalidate must
   * instead leave the previous good entry in place, which is what not calling
   * `set` on the error path achieves.
   */
  set<T>(key: string, data: T): T {
    const entry: CacheEntry<unknown> = { data, storedAt: Date.now() };
    this.entries.delete(key);
    this.entries.set(key, entry);
    this.evict();
    return data;
  }

  /**
   * Share one in-flight request per key. The promise is removed on settle
   * (including rejection) so a failure is immediately retryable rather than
   * every later caller inheriting the same rejected promise.
   *
   * `fresh: true` refuses to join an existing flight and starts its own.
   * Required for anything that must observe state AFTER a mutation: a Refresh
   * click, or a re-list following a delete. Joining a request that was issued
   * BEFORE the write returns the pre-write snapshot, and `set` then caches it —
   * so the deleted row reappears and stays for the whole fresh window. The
   * shared entry is left alone rather than overwritten, so the earlier caller
   * still gets the answer it was waiting for.
   */
  async dedupe<T>(key: string, fetcher: () => Promise<T>, fresh = false): Promise<T> {
    if (fresh) return fetcher();
    const running = this.inflight.get(key);
    if (running) return running as Promise<T>;
    const p = fetcher();
    this.inflight.set(key, p as Promise<unknown>);
    try {
      return await p;
    } finally {
      this.inflight.delete(key);
    }
  }

  /**
   * Drop every entry whose key starts with `prefix`, returning how many went.
   * Call after a mutation invalidates a list — resolving an issue should drop
   * `issues.list` so the next visit refetches instead of showing the old status
   * for up to a minute.
   *
   * Prefix-matches on the raw key string, so passing a bare view name
   * (`'issues.list'`) clears that view across every scope and filter
   * combination. That is the intended blast radius: a mutation's effect on
   * other scopes' cached rows is not knowable from here, and over-invalidating
   * costs one refetch while under-invalidating shows wrong data.
   */
  invalidate(prefix: string): number {
    let dropped = 0;
    for (const key of [...this.entries.keys()]) {
      if (key.startsWith(prefix)) {
        this.entries.delete(key);
        dropped++;
      }
    }
    // In-flight requests matching the prefix are dropped too. They were issued
    // BEFORE the mutation that prompted this call, so their responses describe
    // the pre-mutation world; leaving them registered lets a later `dedupe`
    // join one and re-cache exactly the stale data this call just removed.
    // Only the bookkeeping is discarded — the promise still settles for whoever
    // is already awaiting it.
    for (const key of [...this.inflight.keys()]) {
      if (key.startsWith(prefix)) this.inflight.delete(key);
    }
    return dropped;
  }

  /**
   * Drop everything, including in-flight bookkeeping. Wired into
   * `authStore.clearLocal()`: identity changing is the one event that
   * invalidates every key at once, and it must not depend on any page
   * remembering to clean up after itself.
   *
   * In-flight entries are cleared too so a request started as the previous
   * user cannot populate the cache after the switch. The promise itself still
   * settles — nothing can cancel it here — but it settles into a `dedupe` map
   * that no longer references it, and the page that started it is being torn
   * down, so its result reaches nothing.
   */
  clear(): void {
    this.entries.clear();
    this.inflight.clear();
  }

  /** Retained entry count. Test/debug aid. */
  get size(): number {
    return this.entries.size;
  }

  private touch(key: string, entry: CacheEntry<unknown>): void {
    this.entries.delete(key);
    this.entries.set(key, entry);
  }

  private evict(): void {
    while (this.entries.size > MAX_ENTRIES) {
      const oldest = this.entries.keys().next();
      if (oldest.done) return;
      this.entries.delete(oldest.value);
    }
  }
}

export const viewCache = new ViewCache();
