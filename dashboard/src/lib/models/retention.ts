/**
 * Retention grid and lifecycle shaping — the pure half of the Retention page.
 *
 * Everything here is deliberately free of Svelte and of the network, because
 * this project has no jsdom and no component-render harness: logic that matters
 * gets tested here, and the components are verified in a real browser. Putting
 * the cell classification in a `.svelte` file would make it untestable.
 */

/** One cohort row as the API sends it. */
export interface Cohort {
  /** ISO date of the cohort's first day (or ISO-week Monday). */
  start: string;
  size: number;
  /**
   * One entry per period. `null` means the period has NOT ELAPSED yet — a
   * different fact from zero, and the whole reason this is `(number | null)[]`
   * rather than `number[]`.
   */
  periods: (number | null)[];
}

export interface RetentionGrid {
  granularity: Granularity;
  as_of: string | null;
  /** False when this app's pre-epoch history has not been backfilled. */
  ready: boolean;
  /**
   * When the server computed this response — it is served stale-while-
   * revalidate, so this can lag `Date.now()` by up to an hour. Null on
   * `ready:false` responses (never cached) and absent from pre-cache builds.
   */
  computed_at?: string | null;
  cohorts: Cohort[];
  /** Present only under `split=errors`: the same cohorts, error-free users. */
  clean?: Cohort[];
}

export interface LifecyclePoint {
  start: string;
  new_users: number;
  returning_users: number;
  resurrected_users: number;
  dormant_users: number;
}

export interface ChurnPerson {
  distinct_id: string;
  last_seen: string;
  first_seen: string;
  events_count: number;
  errors_count: number;
  sessions_count: number;
}

export type Granularity = 'day' | 'week';

/**
 * Retention as a rate in 0..1, or `null` when there is no answer.
 *
 * Two distinct reasons for `null`, collapsed on purpose because the UI treats
 * them identically — an empty cell:
 *
 * - the period has not elapsed (`users === null`), and
 * - the cohort is empty, so the rate is 0/0 rather than 0.
 *
 * Never returns 0 for either. The bug this exists to prevent is `users ?? 0`
 * somewhere in a template, which renders a cohort that simply has not aged yet
 * as a catastrophic 0% — indistinguishable from total churn.
 */
export function retentionRate(users: number | null, size: number): number | null {
  if (users === null) return null;
  if (size <= 0) return null;
  return users / size;
}

/** How a single cell should be drawn. */
export type CellKind = 'size' | 'rate' | 'empty';

/**
 * Classify a cell.
 *
 * Period 0 is 100% by construction — everyone in a cohort was, definitionally,
 * active in the period they joined — so it renders the cohort SIZE instead. A
 * column of "100%" is noise that pushes the informative columns off-screen.
 */
export function cellKind(period: number, users: number | null, size: number): CellKind {
  if (users === null) return 'empty';
  if (size <= 0) return 'empty';
  if (period === 0) return 'size';
  return 'rate';
}

/**
 * Colour ramp step, 0..4, for a rate. `null` rates have no step — callers must
 * branch on [`cellKind`] first rather than defaulting a `null` to step 0, which
 * would paint "not yet known" the same as "nobody came back".
 */
export function rampStep(rate: number): number {
  if (rate <= 0) return 0;
  if (rate < 0.1) return 1;
  if (rate < 0.25) return 2;
  if (rate < 0.5) return 3;
  return 4;
}

/**
 * The widest `periods` array across all cohorts — the grid's column count.
 *
 * Cohorts are ragged in principle (the API pads them, but a future caller may
 * not), and a table built from `cohorts[0].periods.length` silently truncates
 * every later row if the first cohort happens to be the shortest.
 */
export function columnCount(cohorts: Cohort[]): number {
  return cohorts.reduce((n, c) => Math.max(n, c.periods.length), 0);
}

/**
 * Stacked-bar series for the lifecycle chart.
 *
 * `dormant` is returned as a NEGATIVE value: it is drawn below the axis, which
 * is the conventional rendering and the only one that reads correctly next to
 * three positive series. The three positive series partition the active set, so
 * their sum is the period's active total — asserted in the tests, because a
 * regression there would double-count people into a taller, wrong bar.
 */
export interface LifecycleBar {
  start: string;
  positive: { key: 'new' | 'returning' | 'resurrected'; value: number }[];
  dormant: number;
  /** Sum of the three positive series — the period's active users. */
  active: number;
}

export function lifecycleBars(points: LifecyclePoint[]): LifecycleBar[] {
  return points.map((p) => ({
    start: p.start,
    positive: [
      { key: 'new' as const, value: p.new_users },
      { key: 'returning' as const, value: p.returning_users },
      { key: 'resurrected' as const, value: p.resurrected_users },
    ],
    dormant: -p.dormant_users,
    active: p.new_users + p.returning_users + p.resurrected_users,
  }));
}

/**
 * Tallest bar in either direction, for the chart's y-scale. Never 0 — a zero
 * denominator would make every bar `NaN%` tall and silently render nothing.
 */
export function lifecycleScale(bars: LifecycleBar[]): number {
  const peak = bars.reduce((m, b) => Math.max(m, b.active, Math.abs(b.dormant)), 0);
  return peak === 0 ? 1 : peak;
}

/**
 * The grid as CSV, raw counts rather than percentages.
 *
 * Raw counts on purpose: a spreadsheet can derive any percentage from
 * `size` + counts, but cannot recover counts from rounded percentages —
 * analysts pivot these into their own decks. Unelapsed periods export as
 * EMPTY fields, never `0`: a downstream `=AVERAGE()` over a column silently
 * treats 0 as data, and a blank as absent, which is exactly the null-vs-zero
 * distinction the wire type carries.
 */
export function gridToCsv(cohorts: Cohort[], granularity: Granularity): string {
  const cols = columnCount(cohorts);
  const unit = granularity === 'week' ? 'week' : 'day';
  const head = ['cohort', 'size', ...Array.from({ length: cols }, (_, n) => `${unit}_${n}_users`)];
  const rows = cohorts.map((c) => [
    c.start,
    String(c.size),
    ...Array.from({ length: cols }, (_, n) => {
      const v = c.periods[n] ?? null;
      return v === null ? '' : String(v);
    }),
  ]);
  return [head, ...rows].map((r) => r.join(',')).join('\n');
}

/** How grid cells render their numbers: share of cohort, or absolute people. */
export type GridMode = 'rate' | 'count';

/**
 * The text for one grid cell, given the display mode.
 *
 * Period-0 cells render `100%` in rate mode — NOT the cohort size. The size
 * already has its own column, and rendering it twice side by side was the
 * "what does Day 0 even mean" confusion in the 2026-08-29 feedback: two
 * identical numbers with different headers explain neither. In count mode
 * period 0 is the size, which there reads as "everyone", consistently with
 * the other cells being people-counts.
 *
 * `fmt` is injectable so the component can pass the locale-aware
 * `formatNumber` (Arabic pins Latin digits) while tests use the default.
 */
export function cellLabel(
  kind: CellKind,
  mode: GridMode,
  users: number | null,
  size: number,
  fmt: (n: number) => string = (n) => n.toLocaleString('en-US'),
): string {
  if (kind === 'empty') return '';
  if (kind === 'size') return mode === 'rate' ? '100%' : fmt(size);
  const rate = retentionRate(users, size);
  if (rate === null || users === null) return '';
  return mode === 'rate' ? `${Math.round(rate * 100)}%` : fmt(users);
}

/**
 * The smallest "nice" number (1, 2 or 5 times a power of ten) at or above
 * `x` — the top tick of an axis. `x <= 0` maps to 1 so a scale always exists.
 */
export function niceCeil(x: number): number {
  if (x <= 1) return 1;
  const pow = 10 ** Math.floor(Math.log10(x));
  for (const m of [1, 2, 5, 10]) {
    if (m * pow >= x) return m * pow;
  }
  return 10 * pow;
}

/**
 * The vertical geometry of the lifecycle chart, computed from data alone.
 *
 * The old chart split its height 50/50 between the active stack and the
 * dormant strip. With dormant a few percent of actives — the normal case —
 * that rendered half the plot as dead space and pushed the date axis far
 * below the bars (the 2026-08-29 screenshot feedback). The two regions now
 * share height in proportion to their scales, with a floor so a nonzero
 * dormant strip stays visible and a cap so a churn catastrophe cannot
 * flatten the actives into a sliver.
 *
 * Everything is fractions and data — no element measurement — so the chart
 * still renders correctly inside a hidden pane (the rAF trap this component
 * has always avoided).
 */
export interface LifecycleLayout {
  /** Axis top value for the positive region — a nice ceiling of the peak. */
  posTop: number;
  /** Axis value for the dormant region's outer edge; 0 when no dormancy. */
  negTop: number;
  /** Fraction of plot height the positive region occupies, 0..1. */
  posShare: number;
  negShare: number;
  /** Ascending tick values for the positive region: [0, posTop/2, posTop]. */
  posTicks: number[];
}

export function lifecycleLayout(bars: LifecycleBar[]): LifecycleLayout {
  const posMax = bars.reduce((m, b) => Math.max(m, b.active), 0);
  const negMax = bars.reduce((m, b) => Math.max(m, -b.dormant), 0);
  const posTop = niceCeil(posMax);
  if (negMax === 0) {
    return { posTop, negTop: 0, posShare: 1, negShare: 0, posTicks: [0, posTop / 2, posTop] };
  }
  const negTop = niceCeil(negMax);
  const raw = negTop / (posTop + negTop);
  const negShare = Math.min(0.5, Math.max(0.08, raw));
  return {
    posTop,
    negTop,
    posShare: 1 - negShare,
    negShare,
    posTicks: [0, posTop / 2, posTop],
  };
}
