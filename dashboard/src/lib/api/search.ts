/**
 * The client half of the search seam: the envelope the searched list endpoints
 * answer, and the one encoder that builds their query strings.
 *
 * Mirrors `backend/bins/sauron-api/src/routes/search.rs`, and exists for the
 * same reason that module does — three routes (`issues::list`,
 * `issues::events`, `analytics::events_list`) return the identical shape from
 * the identical vocabulary, and three hand-rolled copies of it would drift.
 * Drift here is two lists that look the same and behave differently.
 */

/**
 * Set when the planner narrowed the time window to keep the query affordable.
 *
 * `field` is the resource's own window column as the handler names it
 * (`last_seen` on Issues, `occurred_at` on occurrences and events — the
 * planner's generic `since` is mapped by each handler), `to` is a day count
 * like `"30d"`, and `reason` is prose meant to be shown.
 *
 * Cannot be derived client-side: it depends on the *query*, unlike the
 * free-text narrowing a caller without `event:read` gets, which is a pure
 * function of their own permissions and therefore carries no field at all.
 */
export interface ClampInfo {
  field: string;
  to: string;
  reason: string;
}

/**
 * The envelope the searched list endpoints answer.
 *
 * `total` is always a number and `total_is_capped` carries the nuance. The
 * server deliberately does not return a display string like `"1204+"` — that
 * would make every caller parse a number back out of it. Rendering the `+` is
 * a display concern, and belongs in whatever shows the count.
 *
 * `total` stops counting at the server's `COUNT_CAP` (10,000) once the plan
 * degrades to a scan, which is exactly when `total_is_capped` is `true`: read
 * the pair as "at least this many", never as an exact figure on its own.
 */
export interface SearchEnvelope<T> {
  data: T[];
  total: number;
  total_is_capped: boolean;
  /** `null` on the last page. */
  next_cursor: string | null;
  /** `null` when the planner left the caller's own window alone. */
  clamped: ClampInfo | null;
}

/**
 * The parameters that decide WHICH ROWS match — the predicate and its window.
 *
 * Split from {@link SearchPageParams} because the split is load bearing on the
 * wire: `/issues/{id}/events/stats` describes the rows
 * `/issues/{id}/events` returns, so the two requests must be built from one
 * predicate or the caption and the table quietly disagree. The stats route
 * shares the list's `EventsQuery` struct server-side and ignores the page
 * fields (`sort`/`cursor`/`limit`) for the same reason: no ordering and no page
 * boundary changes a total.
 */
export interface SearchPredicateParams {
  /** Pre-language `field:op:value` strings. Bridged server-side into the same AST `query` produces. */
  filters?: string[];
  /** Pre-language free text. Bridged the same way. */
  q?: string;
  /** The query language. Wins outright over `filters`/`q` when non-empty. */
  query?: string;
  sinceDays?: number;
  /**
   * Which timestamp column the window applies to.
   *
   * Validated per route against a whitelist; an unlisted value is a 400 that
   * names the allowed set, not a silently ignored parameter. Omit it to accept
   * the route's default column. Build these three from a `TimeFilterState` via
   * `models/time-filter`'s `toRecord`, never by hand — the precedence rule
   * below is easy to get wrong at a call site.
   */
  timeField?: string;
  /** RFC3339 UTC, inclusive lower bound. Suppresses `sinceDays` server-side. */
  from?: string;
  /** RFC3339 UTC, **exclusive** upper bound. Suppresses `sinceDays` server-side. */
  to?: string;
}

/**
 * The parameters that decide WHICH SLICE of the matching rows comes back.
 *
 * There is deliberately no `offset`. The backend still *accepts* one so an old
 * bookmark does not 400, but it ignores it — keyset paging replaced it, because
 * an offset cannot page a list stably. Sending a parameter the server ignores
 * would put a request on the wire that reads as "rows 50-100" and answers with
 * rows 0-50, so the clients stopped sending it. Follow `next_cursor` instead.
 */
export interface SearchPageParams {
  /**
   * `column` or `-column` for ascending. Restricted per route to orderings with
   * a supporting keyset index; anything else is a 400 that names what is
   * allowed. See each client for its route's set.
   */
  sort?: string;
  /** Opaque token from the previous page's `next_cursor`. */
  cursor?: string;
  limit?: number;
}

export type SearchParams = SearchPredicateParams & SearchPageParams;

/**
 * Encode the predicate half. Every value is written through `URLSearchParams`,
 * so nothing here needs its own escaping.
 *
 * Empty strings are dropped rather than sent: the server normalises an empty
 * `q=`/`query=` to "absent" anyway, and sending them makes a request that reads
 * as a search that ran.
 */
export function predicateParams(opts: SearchPredicateParams): URLSearchParams {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.query) p.set('query', opts.query);
  if (opts.timeField) p.set('time_field', opts.timeField);
  if (opts.from) p.set('from', opts.from);
  if (opts.to) p.set('to', opts.to);
  // Sent only when neither bound is present. The server ignores `since_days`
  // whenever a bound is set, so including it anyway would put a request on the
  // wire that reads as two conflicting windows — and would make a bookmarked
  // URL look like it carried a window it does not.
  if (opts.sinceDays != null && !opts.from && !opts.to) {
    p.set('since_days', String(opts.sinceDays));
  }
  return p;
}

/** Append the page half to `p`, in place, and return it. */
export function appendPageParams(p: URLSearchParams, opts: SearchPageParams): URLSearchParams {
  if (opts.sort) p.set('sort', opts.sort);
  if (opts.cursor) p.set('cursor', opts.cursor);
  if (opts.limit != null) p.set('limit', String(opts.limit));
  return p;
}

/** Both halves — what a list request sends. */
export function searchParams(opts: SearchParams): URLSearchParams {
  return appendPageParams(predicateParams(opts), opts);
}
