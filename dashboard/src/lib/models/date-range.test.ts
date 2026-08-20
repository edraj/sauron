import { describe, expect, it } from 'vitest';
import {
  MAX_RANGE_DAYS,
  MAX_RELATIVE_DAYS,
  customRange,
  dayRange,
  fromParams,
  isValidRange,
  lastDays,
  monthRange,
  rangeKey,
  spanDays,
  toParams,
  toPredicate,
  formatAbsolute,
  weekRange,
  type DateRangeValue,
} from './date-range';

/** Local midnight, so assertions read in the same zone the builders work in. */
function localDay(y: number, m: number, d: number): Date {
  return new Date(y, m - 1, d, 12, 0, 0, 0);
}

/** What a local-midnight instant serialises to, for comparison. */
function localMidnightIso(y: number, m: number, d: number): string {
  return new Date(y, m - 1, d, 0, 0, 0, 0).toISOString();
}

describe('builders', () => {
  it('a day spans that local day and stops at the next midnight', () => {
    const r = dayRange(localDay(2026, 8, 12));
    expect(r.kind).toBe('absolute');
    if (r.kind !== 'absolute') return;
    expect(r.preset).toBe('day');
    expect(r.from).toBe(localMidnightIso(2026, 8, 12));
    expect(r.to).toBe(localMidnightIso(2026, 8, 13));
  });

  /**
   * The convention `time-filter.ts` already documents: `to` is EXCLUSIVE, so a
   * day ends at the FOLLOWING midnight. Ending it at its own midnight would
   * select nothing at all — the single easiest way to ship a picker that
   * always shows an empty page.
   */
  it('never produces an empty window', () => {
    const r = dayRange(localDay(2026, 8, 12));
    if (r.kind !== 'absolute') throw new Error('absolute');
    expect(new Date(r.to).getTime()).toBeGreaterThan(new Date(r.from).getTime());
  });

  it('a month spans exactly that calendar month', () => {
    const r = monthRange(localDay(2026, 7, 17));
    if (r.kind !== 'absolute') throw new Error('absolute');
    expect(r.preset).toBe('month');
    expect(r.from).toBe(localMidnightIso(2026, 7, 1));
    expect(r.to).toBe(localMidnightIso(2026, 8, 1));
  });

  it('a December month rolls over into the next year', () => {
    const r = monthRange(localDay(2026, 12, 25));
    if (r.kind !== 'absolute') throw new Error('absolute');
    expect(r.from).toBe(localMidnightIso(2026, 12, 1));
    expect(r.to).toBe(localMidnightIso(2027, 1, 1));
  });

  it('a week starts on the configured day and spans seven of them', () => {
    // 12 Aug 2026 is a Wednesday. Monday-start puts the week on the 10th.
    const mon = weekRange(localDay(2026, 8, 12), 1);
    if (mon.kind !== 'absolute') throw new Error('absolute');
    expect(mon.preset).toBe('week');
    expect(mon.from).toBe(localMidnightIso(2026, 8, 10));
    expect(mon.to).toBe(localMidnightIso(2026, 8, 17));

    const sun = weekRange(localDay(2026, 8, 12), 0);
    if (sun.kind !== 'absolute') throw new Error('absolute');
    expect(sun.from).toBe(localMidnightIso(2026, 8, 9));
    expect(sun.to).toBe(localMidnightIso(2026, 8, 16));
  });

  it('a week containing the first of a month reaches back into the previous one', () => {
    // 1 Aug 2026 is a Saturday; its Monday-start week begins 27 July.
    const r = weekRange(localDay(2026, 8, 1), 1);
    if (r.kind !== 'absolute') throw new Error('absolute');
    expect(r.from).toBe(localMidnightIso(2026, 7, 27));
  });

  it('a custom range covers all of its last day', () => {
    const r = customRange(localDay(2026, 8, 10), localDay(2026, 8, 14));
    if (r.kind !== 'absolute') throw new Error('absolute');
    expect(r.preset).toBe('custom');
    expect(r.from).toBe(localMidnightIso(2026, 8, 10));
    // 15th, not 14th — otherwise the whole final day is dropped, which reads
    // as a data bug rather than a boundary convention.
    expect(r.to).toBe(localMidnightIso(2026, 8, 15));
  });

  it('a custom range accepts its endpoints in either order', () => {
    const forward = customRange(localDay(2026, 8, 10), localDay(2026, 8, 14));
    const reversed = customRange(localDay(2026, 8, 14), localDay(2026, 8, 10));
    expect(reversed).toEqual(forward);
  });

  it('a single-day custom range is the same as a day range', () => {
    expect(customRange(localDay(2026, 8, 10), localDay(2026, 8, 10))).toEqual({
      ...dayRange(localDay(2026, 8, 10)),
      preset: 'custom',
    });
  });
});

describe('validation', () => {
  it('accepts the shapes the builders produce', () => {
    expect(isValidRange(lastDays(30))).toBe(true);
    expect(isValidRange(dayRange(localDay(2026, 8, 12)))).toBe(true);
  });

  it('rejects anything that is not one of the two shapes', () => {
    for (const bad of [null, undefined, 42, 'last', {}, { kind: 'nope' }, []]) {
      expect(isValidRange(bad)).toBe(false);
    }
  });

  it('rejects a relative window outside 1..MAX_RELATIVE_DAYS', () => {
    expect(isValidRange({ kind: 'last', days: 0 })).toBe(false);
    expect(isValidRange({ kind: 'last', days: -1 })).toBe(false);
    expect(isValidRange({ kind: 'last', days: 1.5 })).toBe(false);
    expect(isValidRange({ kind: 'last', days: MAX_RELATIVE_DAYS + 1 })).toBe(false);
    expect(isValidRange({ kind: 'last', days: MAX_RELATIVE_DAYS })).toBe(true);
  });

  /**
   * The asymmetry, pinned. `Issues` ships 3650 as its widest setting and the
   * server silently clamps it to 365; rejecting it here would break a control
   * that has always worked. An ABSOLUTE window that wide is refused by the
   * server, so it is refused here — see the next test.
   */
  it('accepts a relative window far wider than an absolute one may be', () => {
    expect(isValidRange({ kind: 'last', days: 3650 })).toBe(true);
    expect(MAX_RELATIVE_DAYS).toBeGreaterThan(MAX_RANGE_DAYS);
  });

  /**
   * Half-open, so equal bounds select nothing. This is the shape a stale
   * localStorage entry could carry, and restoring it would show an empty
   * dashboard with no explanation.
   */
  it('rejects an inverted or empty absolute window', () => {
    const base = { kind: 'absolute' as const, preset: 'custom' as const };
    expect(
      isValidRange({ ...base, from: '2026-08-05T00:00:00.000Z', to: '2026-08-01T00:00:00.000Z' }),
    ).toBe(false);
    expect(
      isValidRange({ ...base, from: '2026-08-05T00:00:00.000Z', to: '2026-08-05T00:00:00.000Z' }),
    ).toBe(false);
  });

  it('rejects an unparseable instant', () => {
    expect(
      isValidRange({ kind: 'absolute', preset: 'day', from: 'yesterday', to: 'today' }),
    ).toBe(false);
  });

  /**
   * The server refuses an explicit window wider than its ceiling rather than
   * narrowing it, so a stored range that exceeds it would 400 every analytics
   * request on the page. Rejecting it here turns that into a fallback.
   */
  it('rejects an absolute window wider than the ceiling', () => {
    const from = new Date(Date.UTC(2020, 0, 1)).toISOString();
    const to = new Date(Date.UTC(2026, 0, 1)).toISOString();
    expect(isValidRange({ kind: 'absolute', preset: 'custom', from, to })).toBe(false);
  });
});

describe('the wire', () => {
  it('sends since_days for a relative window and no bounds', () => {
    expect(toParams(lastDays(7))).toEqual({ since_days: '7' });
  });

  /**
   * `since_days` is not sent alongside a bound. The server ignores it whenever
   * either bound is present, so sending it anyway puts a request on the wire
   * that reads as two conflicting windows.
   */
  it('sends only the bounds for an absolute window', () => {
    const r = dayRange(localDay(2026, 8, 12));
    if (r.kind !== 'absolute') throw new Error('absolute');
    expect(toParams(r)).toEqual({ from: r.from, to: r.to });
  });

  it('round-trips the BOUNDS through query parameters', () => {
    for (const v of [
      lastDays(7),
      dayRange(localDay(2026, 8, 12)),
      monthRange(localDay(2026, 7, 3)),
    ]) {
      const sp = new URLSearchParams(toParams(v));
      // The preset is a label and does not travel — see the next test. What
      // must survive is the window itself.
      expect(fromParams(sp, 30)).toEqual(v.kind === 'last' ? v : { ...v, preset: 'custom' });
    }
  });

  /**
   * The preset is not on the wire — it is a label, not a filter — so a decoded
   * absolute range comes back as `custom`. Asserted rather than left implicit
   * because a URL is the one place a range crosses a process boundary.
   */
  it('decodes an absolute window as custom', () => {
    const sp = new URLSearchParams({
      from: '2026-08-12T00:00:00.000Z',
      to: '2026-08-13T00:00:00.000Z',
    });
    expect(fromParams(sp, 30)).toEqual({
      kind: 'absolute',
      preset: 'custom',
      from: '2026-08-12T00:00:00.000Z',
      to: '2026-08-13T00:00:00.000Z',
    });
  });

  it('falls back rather than failing on a stale or hand-edited URL', () => {
    expect(fromParams(new URLSearchParams('since_days=abc'), 30)).toEqual(lastDays(30));
    expect(fromParams(new URLSearchParams('since_days=99999'), 30)).toEqual(lastDays(30));
    // 3650 is a value `Issues` really sends, so it must survive.
    expect(fromParams(new URLSearchParams('since_days=3650'), 30)).toEqual(lastDays(3650));
    expect(fromParams(new URLSearchParams('from=nonsense&to=also'), 90)).toEqual(lastDays(90));
    // Inverted bounds are a 400 at the server; a bookmark must degrade to a
    // view, not to an error page.
    expect(
      fromParams(
        new URLSearchParams('from=2026-08-09T00:00:00.000Z&to=2026-08-01T00:00:00.000Z'),
        30,
      ),
    ).toEqual(lastDays(30));
  });
});

describe('derived values', () => {
  it('spanDays counts whole days in either shape', () => {
    expect(spanDays(lastDays(7))).toBe(7);
    expect(spanDays(dayRange(localDay(2026, 8, 12)))).toBe(1);
    expect(spanDays(monthRange(localDay(2026, 7, 3)))).toBe(31);
  });

  /**
   * The cache key every page builds its `viewKey` from. A key derived from the
   * clock would mint a fresh entry per load and hit zero times — the
   * `CachedView` trap this codebase has already shipped once.
   */
  it('rangeKey is stable for the same selection and distinct across selections', () => {
    expect(rangeKey(lastDays(30))).toBe(rangeKey(lastDays(30)));
    expect(rangeKey(lastDays(30))).not.toBe(rangeKey(lastDays(7)));
    const day = dayRange(localDay(2026, 8, 12));
    expect(rangeKey(day)).toBe(rangeKey(dayRange(localDay(2026, 8, 12))));
    expect(rangeKey(day)).not.toBe(rangeKey(lastDays(1)));
  });

  it('rangeKey does not collide a preset with a same-length absolute window', () => {
    const thirty: DateRangeValue = {
      kind: 'absolute',
      preset: 'custom',
      from: '2026-07-09T00:00:00.000Z',
      to: '2026-08-08T00:00:00.000Z',
    };
    expect(rangeKey(thirty)).not.toBe(rangeKey(lastDays(30)));
  });
});

describe('labels', () => {
  it('names a month without spelling out its days', () => {
    expect(formatAbsolute(monthRange(localDay(2026, 7, 17)), 'en')).toBe('July 2026');
  });

  it('names a single day', () => {
    expect(formatAbsolute(dayRange(localDay(2026, 8, 12)), 'en')).toBe('Aug 12, 2026');
  });

  /**
   * The end shown is INCLUSIVE. `to` is the exclusive next-midnight, so
   * printing it raw would caption a 10th-to-14th selection as "10 – 15 Aug" —
   * a label that contradicts what the user clicked.
   */
  it('names a span by its last INCLUDED day', () => {
    const label = formatAbsolute(customRange(localDay(2026, 8, 10), localDay(2026, 8, 14)), 'en');
    expect(label).toContain('10');
    expect(label).toContain('14');
    expect(label).not.toContain('15');
  });

  it('follows the locale it is given', () => {
    // Not asserting the exact Arabic string — the point is that the formatter
    // is reached at all, so a label cannot be frozen in English.
    expect(formatAbsolute(monthRange(localDay(2026, 7, 17)), 'ar-u-nu-latn')).not.toBe('July 2026');
  });
});

describe('the list-client adapter', () => {
  it('hands a relative window over as a number', () => {
    expect(toPredicate(lastDays(7))).toEqual({ sinceDays: 7 });
  });

  /**
   * Never both. `predicateParams` drops `sinceDays` when a bound is present,
   * but a caller that sent both would still have described two windows.
   */
  it('hands an absolute window over as bounds alone', () => {
    const r = dayRange(localDay(2026, 8, 12));
    expect(toPredicate(r)).toEqual({ from: r.from, to: r.to });
  });
});
