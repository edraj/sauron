import { describe, it, expect } from 'vitest';
import { insightLink, retentionInsights } from './retention-insights';
import { PAGE_ACCESS } from './page-access';
import type { Cohort, LifecyclePoint } from './retention';

const cohort = (start: string, size: number, day1: number | null): Cohort => ({
  start,
  size,
  periods: [size, day1],
});

const point = (
  start: string,
  n: number,
  r: number,
  z: number,
  d: number,
): LifecyclePoint => ({
  start,
  new_users: n,
  returning_users: r,
  resurrected_users: z,
  dormant_users: d,
});

describe('retentionInsights', () => {
  it('returns nothing on no data rather than guessing', () => {
    expect(retentionInsights([], [])).toEqual([]);
  });

  it('flags declining day-1 retention as bad', () => {
    const cohorts = [
      cohort('2026-08-01', 100, 10),
      cohort('2026-08-02', 100, 9),
      cohort('2026-08-03', 100, 4),
      cohort('2026-08-04', 100, 3),
    ];
    const found = retentionInsights(cohorts, []);
    expect(found.some((i) => i.key === 'retention.insight.day1Down')).toBe(true);
  });

  it('calls out churn-and-replace when actives are overwhelmingly new', () => {
    const points = [point('2026-08-20', 90, 5, 5, 10), point('2026-08-21', 95, 3, 2, 12)];
    const found = retentionInsights([], points);
    expect(found.some((i) => i.key === 'retention.insight.churnReplace')).toBe(true);
  });

  it('computes the quick ratio and its tone from gained vs lost', () => {
    const gaining = retentionInsights([], [point('2026-08-20', 30, 10, 10, 20)]);
    expect(gaining.some((i) => i.key === 'retention.insight.quickGood')).toBe(true);
    const shrinking = retentionInsights([], [point('2026-08-20', 5, 10, 0, 50)]);
    expect(shrinking.some((i) => i.key === 'retention.insight.quickBad')).toBe(true);
  });

  it('names the cliff period where everyone went silent', () => {
    const points = [point('2026-08-20', 10, 0, 0, 0), point('2026-08-21', 0, 0, 0, 10)];
    const found = retentionInsights([], points);
    const cliff = found.find((i) => i.key === 'retention.insight.cliff');
    expect(cliff?.params?.date).toBe('2026-08-21');
    expect(cliff?.tone).toBe('bad');
  });

  it('skips the best-cohort call-out for tiny cohorts', () => {
    // 3 people retaining well is an anecdote, not a benchmark.
    const found = retentionInsights([cohort('2026-08-01', 3, 2), cohort('2026-08-02', 3, 1)], []);
    expect(found.some((i) => i.key === 'retention.insight.bestCohort')).toBe(false);
  });

  it('orders findings most-severe first', () => {
    const cohorts = [
      cohort('2026-08-01', 100, 10),
      cohort('2026-08-02', 100, 9),
      cohort('2026-08-03', 100, 4),
      cohort('2026-08-04', 100, 3),
    ];
    const points = [point('2026-08-20', 30, 10, 5, 20)];
    const tones = retentionInsights(cohorts, points).map((i) => i.tone);
    const order = { bad: 0, warn: 1, good: 2, info: 3 } as const;
    const ranks = tones.map((t) => order[t]);
    expect([...ranks].sort((a, b) => a - b)).toEqual(ranks);
  });
});

describe('day-1 trend yardsticks', () => {
  it('flags a large RELATIVE slide even when absolute points are small', () => {
    // 3.2% -> 2.0% is a third of the base gone — the production shape from
    // the 2026-08-29 screenshot, which the absolute-only threshold called
    // flat.
    const cohorts = [
      cohort('2026-08-18', 100000, 3200),
      cohort('2026-08-19', 100000, 3200),
      cohort('2026-08-20', 100000, 2000),
      cohort('2026-08-21', 100000, 2000),
    ];
    const found = retentionInsights(cohorts, []);
    expect(found.some((i) => i.key === 'retention.insight.day1Down')).toBe(true);
  });

  it('keeps a sub-half-point wiggle flat regardless of ratio', () => {
    // 0.30% -> 0.20% is a big ratio on a tiny base; still noise.
    const cohorts = [
      cohort('2026-08-18', 100000, 300),
      cohort('2026-08-19', 100000, 300),
      cohort('2026-08-20', 100000, 200),
      cohort('2026-08-21', 100000, 200),
    ];
    const found = retentionInsights(cohorts, []);
    expect(found.some((i) => i.key === 'retention.insight.day1Flat')).toBe(true);
  });
});

describe('recommended actions', () => {
  // A spread wide enough to trigger every branch at least once.
  const cohorts = [
    cohort('2026-08-01', 100, 10),
    cohort('2026-08-02', 100, 9),
    cohort('2026-08-03', 100, 4),
    cohort('2026-08-04', 100, 3),
  ];
  const points = [
    point('2026-08-20', 95, 3, 2, 10),
    point('2026-08-21', 0, 0, 0, 40),
    point('2026-08-22', 30, 10, 5, 20),
  ];
  const all = retentionInsights(cohorts, points);

  it('gives EVERY finding an action — a finding with no next step is the bug', () => {
    expect(all.length).toBeGreaterThan(0);
    for (const i of all) {
      expect(i.actionKey, `${i.key} has no action`).toMatch(/^retention\.action\./);
    }
  });

  it('derives the action key from the finding, so advice cannot be misattached', () => {
    for (const i of all) {
      const name = i.key.slice(i.key.lastIndexOf('.') + 1);
      expect(i.actionKey).toBe(`retention.action.${name}`);
      if (i.link) expect(i.link.labelKey).toBe(`retention.actionLink.${name}`);
    }
  });

  it('only links to routes that actually exist in PAGE_ACCESS', () => {
    // A typo'd route renders a link that falls through to the router's
    // catch-all — a dead end that no type checks.
    for (const i of all) {
      if (!i.link) continue;
      expect(Object.keys(PAGE_ACCESS), `${i.key} -> ${i.link.route}`).toContain(i.link.route);
    }
  });

  it('withholds the shortcut, never the advice, when the page is off-limits', () => {
    const linked = all.find((i) => i.link);
    expect(linked, 'fixture produced no linked finding').toBeDefined();
    expect(insightLink(linked!, () => false)).toBeNull();
    expect(insightLink(linked!, () => true)).toEqual(linked!.link);
    // The finding itself is untouched either way — the analysis is not gated.
    expect(linked!.actionKey).toMatch(/^retention\.action\./);
  });

  it('offers no link for a finding whose next step is on this page', () => {
    // insightLink returns null for an unlinked finding regardless of access,
    // so the caller never has to special-case it.
    const unlinked = { tone: 'info' as const, key: 'retention.insight.x', actionKey: 'retention.action.x' };
    expect(insightLink(unlinked, () => true)).toBeNull();
  });
});

describe('catalog parity for derived keys', () => {
  // `withAction` builds i18n keys by CONCATENATION, so TypeScript cannot check
  // them and the page's `t(... as never)` casts erase what is left. `t()` then
  // falls back to printing the raw key, so a missing entry ships as the literal
  // string "retention.action.cliff" on a production page. This is the only
  // thing standing between that and a release.
  it('has an English and Arabic entry for every action key a finding can emit', async () => {
    const { MESSAGES } = await import('../i18n/catalog');
    const { LOCALES } = await import('../i18n/types');
    const catalog = MESSAGES as Record<string, Record<string, string> | undefined>;

    // Every branch of every metric, so no finding escapes the check.
    const universe = [
      ...retentionInsights(
        [cohort('2026-08-01', 100, 10), cohort('2026-08-02', 100, 9), cohort('2026-08-03', 100, 4), cohort('2026-08-04', 100, 3)],
        [point('2026-08-20', 95, 3, 2, 10), point('2026-08-21', 0, 0, 0, 40)],
      ),
      ...retentionInsights(
        [cohort('2026-08-01', 100, 3), cohort('2026-08-02', 100, 4), cohort('2026-08-03', 100, 9), cohort('2026-08-04', 100, 10)],
        [point('2026-08-20', 30, 30, 30, 10)],
      ),
      ...retentionInsights(
        [cohort('2026-08-01', 100, 30), cohort('2026-08-02', 100, 30)],
        [point('2026-08-20', 5, 40, 0, 90)],
      ),
    ];
    const emitted = new Set(universe.map((i) => i.key.slice(i.key.lastIndexOf('.') + 1)));
    // The fixtures above must actually reach every branch, or this test passes
    // by covering nothing.
    expect([...emitted].sort()).toEqual(
      ['bestCohort', 'churnReplace', 'cliff', 'day1Down', 'day1Flat', 'day1Up', 'quickBad', 'quickGood'].sort(),
    );

    for (const i of universe) {
      for (const key of [i.actionKey, ...(i.link ? [i.link.labelKey] : [])]) {
        const entry = catalog[key];
        expect(entry, `missing catalog entry: ${key}`).toBeDefined();
        for (const locale of LOCALES) {
          expect(entry?.[locale], `${key} has no ${locale} translation`).toBeTruthy();
        }
      }
    }
  });
});
