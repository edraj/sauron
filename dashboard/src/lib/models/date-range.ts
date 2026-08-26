/**
 * The window the range picker selects: either a rolling "last N days" or an
 * absolute pair of instants.
 *
 * Pure — no Svelte imports, no network, and no clock read outside the one
 * function that documents one. The persisted store (`stores/range.svelte.ts`)
 * and the picker component both build on this, so every rule about what a
 * window IS lives here and is unit-testable without a DOM.
 *
 * Mirrors the server's `search::resolve_range`, which is the authority on all
 * of it. This module's job is to avoid sending a request the server would
 * reject, not to re-decide the semantics — with one deliberate addition: the
 * server REFUSES an explicit window wider than its ceiling (it has no envelope
 * to disclose a narrowing through), so this refuses to build one.
 */

/** Which calendar unit produced an absolute window. A LABEL, not a filter. */
export type AbsolutePreset = 'day' | 'week' | 'month' | 'custom';

export interface LastRange {
  readonly kind: 'last';
  /** Whole days in `[1, MAX_RELATIVE_DAYS]`. */
  readonly days: number;
}

export interface AbsoluteRange {
  readonly kind: 'absolute';
  /** RFC3339 UTC. INCLUSIVE lower bound. */
  readonly from: string;
  /**
   * RFC3339 UTC. **EXCLUSIVE** upper bound.
   *
   * The same convention `TimeFilterState` documents, for the same reason: an
   * inclusive bound would have to be spelled as the last representable instant
   * of the period, and `timestamptz` stores microseconds, so `23:59:59.999`
   * silently drops the final millisecond of every window.
   */
  readonly to: string;
  /**
   * Stored, while the human-readable label is NOT.
   *
   * A label baked into localStorage in English would still read English after
   * the user switches the dashboard to Arabic. Keeping the preset lets the
   * label be derived at render time, in whatever locale is active.
   */
  readonly preset: AbsolutePreset;
}

export type DateRangeValue = LastRange | AbsoluteRange;

/**
 * The widest window the analytics routes serve. Mirrors `MAX_WINDOW_DAYS`.
 *
 * Note `Issues` still sends a `since_days` far above this and is silently
 * served 365 — that path is unchanged and deliberately so (see
 * `resolve_range`'s doc comment). Only ABSOLUTE windows are bounded here.
 */
export const MAX_RANGE_DAYS = 365;

/**
 * The ceiling on a RELATIVE window, which is deliberately far higher.
 *
 * The asymmetry mirrors the server exactly. `since_days` above the route
 * ceiling is silently clamped to 365 — `Issues` ships 3650 as its widest
 * setting and has always been served 365 — so rejecting it here would break a
 * shipped control. `from`/`to` above the ceiling are REFUSED, because those
 * routes return bare arrays with nowhere to disclose a narrowing, so a value
 * this side lets through would 400 every request on the page.
 *
 * See `search::resolve_range`'s doc comment, which is the authority.
 */
export const MAX_RELATIVE_DAYS = 3650;

const DAY_MS = 86_400_000;

/** The plain rolling window the preset chips select. */
export function lastDays(days: number): LastRange {
  return { kind: 'last', days };
}

// ---------------------------------------------------------------------------
// Builders — local calendar in, UTC instants out
// ---------------------------------------------------------------------------

/**
 * Local midnight starting the day `d` falls in, offset by `plusDays`.
 *
 * Calendar arithmetic through the `Date` constructor's own overflow handling,
 * never `+ 86_400_000`: a day is 23 hours across a spring-forward transition
 * and 25 across a fall-back, so the millisecond form lands in the wrong day and
 * quietly widens or narrows the window. `time-filter.ts` documents the same
 * rule one module over.
 */
function midnight(d: Date, plusDays = 0): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + plusDays, 0, 0, 0, 0);
}

function absolute(from: Date, to: Date, preset: AbsolutePreset): AbsoluteRange {
  return { kind: 'absolute', from: from.toISOString(), to: to.toISOString(), preset };
}

/** One local day: `[midnight, next midnight)`. */
export function dayRange(d: Date): AbsoluteRange {
  return absolute(midnight(d), midnight(d, 1), 'day');
}

/**
 * The local week containing `d`.
 *
 * `weekStartsOn` is 0 (Sunday) … 6, matching `Date.getDay()`. It is a parameter
 * rather than a constant because the answer is locale-dependent — Sunday in
 * `en-US`, Monday in most of Europe, Saturday in much of the Arabic-speaking
 * world — and this module has no business reading the active locale.
 */
export function weekRange(d: Date, weekStartsOn: number): AbsoluteRange {
  const back = (d.getDay() - weekStartsOn + 7) % 7;
  const start = midnight(d, -back);
  return absolute(start, midnight(start, 7), 'week');
}

/** The local calendar month containing `d`. */
export function monthRange(d: Date): AbsoluteRange {
  const start = new Date(d.getFullYear(), d.getMonth(), 1, 0, 0, 0, 0);
  // Month + 1 with day 1 — the constructor rolls December into January of the
  // following year, so no year arithmetic is needed here.
  const end = new Date(d.getFullYear(), d.getMonth() + 1, 1, 0, 0, 0, 0);
  return absolute(start, end, 'month');
}

/**
 * An arbitrary span between two local days, both INCLUSIVE as the user means
 * them.
 *
 * `to` therefore becomes the start of the FOLLOWING day: "10 Aug to 14 Aug"
 * covers all of 14 August. Truncating to the start of its own day would drop
 * the whole final day — a boundary convention that reads as a data bug.
 *
 * The two arguments are sorted, because a calendar lets you click the end
 * before the start and refusing that would be a worse control than accepting
 * it.
 */
export function customRange(a: Date, b: Date): AbsoluteRange {
  const [lo, hi] = midnight(a).getTime() <= midnight(b).getTime() ? [a, b] : [b, a];
  return absolute(midnight(lo), midnight(hi, 1), 'custom');
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

const PRESETS: readonly string[] = ['day', 'week', 'month', 'custom'];

function instant(v: unknown): number | null {
  if (typeof v !== 'string') return null;
  const t = new Date(v).getTime();
  return Number.isNaN(t) ? null : t;
}

/**
 * Whether `v` is a window this app can both render and send.
 *
 * Used on every read from localStorage and from a URL — both of which outlive
 * the code that wrote them, and neither of which is trustworthy. A rejected
 * value falls back to the page default rather than surfacing an error: a stale
 * bookmark must degrade to a view, not to an error page.
 */
export function isValidRange(v: unknown): v is DateRangeValue {
  if (typeof v !== 'object' || v === null) return false;
  // A structural probe rather than `Partial<LastRange & AbsoluteRange>`: the
  // intersection makes `kind` both `'last'` and `'absolute'`, which resolves to
  // `never` and takes every other field with it.
  const r = v as { kind?: unknown; days?: unknown; from?: unknown; to?: unknown; preset?: unknown };
  if (r.kind === 'last') {
    return (
      typeof r.days === 'number' &&
      Number.isInteger(r.days) &&
      r.days >= 1 &&
      r.days <= MAX_RELATIVE_DAYS
    );
  }
  if (r.kind !== 'absolute') return false;
  if (typeof r.preset !== 'string' || !PRESETS.includes(r.preset)) return false;
  const from = instant(r.from);
  const to = instant(r.to);
  if (from === null || to === null) return false;
  // `>=`, not `>`: half-open, so equal bounds select nothing at all.
  if (from >= to) return false;
  // The server refuses rather than narrows an over-wide explicit window, so a
  // stored range past the ceiling would 400 every request on the page.
  return to - from <= MAX_RANGE_DAYS * DAY_MS;
}

// ---------------------------------------------------------------------------
// The wire, and the URL — one encoding for both
// ---------------------------------------------------------------------------

/**
 * Encode a window as query parameters.
 *
 * `since_days` is sent ONLY for a relative window: the server ignores it
 * whenever a bound is present, so sending it anyway would put a request on the
 * wire that reads as two conflicting windows.
 */
export function toParams(v: DateRangeValue): Record<string, string> {
  return v.kind === 'last' ? { since_days: String(v.days) } : { from: v.from, to: v.to };
}

/**
 * The same window, shaped for a JSON request BODY.
 *
 * Separate from [`toParams`] because the two wires type values differently. A
 * query string carries only text, so `since_days=30` arrives as a string and
 * the server's query deserializer parses it. A JSON body preserves types, and
 * the server deserializes `since_days` there as an `i64` — so the string
 * `toParams` emits is a 422 ("invalid type: string \"30\", expected i64"), not
 * a number. Spreading `toParams` into a POST body is exactly the mistake that
 * shipped: the funnel page sent `{"since_days":"30"}` and every relative
 * window — including the default — failed while the absolute ones worked.
 */
export function toBody(v: DateRangeValue): { since_days: number } | { from: string; to: string } {
  return v.kind === 'last' ? { since_days: v.days } : { from: v.from, to: v.to };
}

/**
 * Decode a window from query parameters, falling back to `fallbackDays` for
 * anything this app cannot honour.
 *
 * The shape is INFERRED from which parameters are present rather than carried
 * as its own field — a `kind` that disagreed with the bounds beside it would be
 * a third source of truth. An absolute window decodes as `custom` because the
 * preset is a label and labels do not travel on the wire.
 */
export function fromParams(sp: URLSearchParams, fallbackDays: number): DateRangeValue {
  const from = sp.get('from');
  const to = sp.get('to');
  if (from && to) {
    const candidate: AbsoluteRange = { kind: 'absolute', from, to, preset: 'custom' };
    if (isValidRange(candidate)) return candidate;
  }
  const raw = sp.get('since_days');
  const n = raw === null ? NaN : Number(raw);
  const days = Number.isInteger(n) && n >= 1 && n <= MAX_RELATIVE_DAYS ? n : fallbackDays;
  return lastDays(days);
}

// ---------------------------------------------------------------------------
// Derived values
// ---------------------------------------------------------------------------

/** Whole days the window covers, rounded up. */
export function spanDays(v: DateRangeValue): number {
  if (v.kind === 'last') return v.days;
  return Math.ceil((new Date(v.to).getTime() - new Date(v.from).getTime()) / DAY_MS);
}

/**
 * A stable, collision-free component for the view cache and the SSE filter.
 *
 * Mirrors the server's `overview_cache::Window::token` deliberately: the two
 * partition the same space, and a client key coarser than the server's would
 * show one window's cached answer under another window's label.
 *
 * Never derived from the clock. `CachedView` shipped a clock-derived `viewKey`
 * once — it minted a fresh entry per load and hit zero times, while compiling,
 * passing every test, and looking perfectly healthy.
 */
export function rangeKey(v: DateRangeValue): string {
  return v.kind === 'last' ? `${v.days}d` : `${v.from}..${v.to}`;
}

/**
 * The bounds the window covers right now, for code that needs concrete
 * instants (chart axes, empty-state copy).
 *
 * The ONE clock read in this module, and it takes `now` as a parameter so the
 * caller decides when. Nothing here feeds a cache key — see [`rangeKey`].
 */
export function bounds(v: DateRangeValue, now: Date = new Date()): { from: Date; to: Date } {
  if (v.kind === 'last') {
    return { from: new Date(now.getTime() - v.days * DAY_MS), to: now };
  }
  return { from: new Date(v.from), to: new Date(v.to) };
}

/**
 * A human label for an absolute window, in `tag`'s locale.
 *
 * Derived at render time from the stored `preset` rather than persisted: a
 * label written into `localStorage` in English would still read English after
 * the user switches the dashboard to Arabic.
 *
 * The end shown is INCLUSIVE — `to` minus one day — because `to` is the
 * exclusive next-midnight, and captioning a range "10–15 Aug" when the user
 * picked the 10th to the 14th would contradict what they clicked.
 */
export function formatAbsolute(v: AbsoluteRange, tag: string): string {
  const from = new Date(v.from);
  const lastDay = new Date(new Date(v.to).getTime() - 1);

  if (v.preset === 'month') {
    return new Intl.DateTimeFormat(tag, { month: 'long', year: 'numeric' }).format(from);
  }

  const sameDayWindow = spanDays(v) <= 1;
  if (v.preset === 'day' || sameDayWindow) {
    return new Intl.DateTimeFormat(tag, {
      day: 'numeric',
      month: 'short',
      year: 'numeric',
    }).format(from);
  }

  // `formatRange` collapses the shared parts itself ("10 – 16 Aug 2026"), which
  // no hand-built concatenation gets right across locales.
  const fmt = new Intl.DateTimeFormat(tag, { day: 'numeric', month: 'short', year: 'numeric' });
  return fmt.formatRange(from, lastDay);
}

/**
 * The same window, shaped for the LIST clients.
 *
 * `models/search`'s `PredicateParams` predates this module and takes
 * `sinceDays` as a NUMBER alongside string `from`/`to` — and it already
 * suppresses `sinceDays` whenever a bound is present, which is the same
 * precedence rule [`toParams`] encodes. This adapts between the two shapes so
 * neither has to learn the other's, and so no page hand-builds a request that
 * carries both.
 */
export function toPredicate(v: DateRangeValue): {
  sinceDays?: number;
  from?: string;
  to?: string;
} {
  return v.kind === 'last' ? { sinceDays: v.days } : { from: v.from, to: v.to };
}
