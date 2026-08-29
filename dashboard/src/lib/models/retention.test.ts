import { describe, it, expect } from 'vitest';
import {
  cellLabel,
  lifecycleLayout,
  niceCeil,
  gridToCsv,
  retentionRate,
  cellKind,
  rampStep,
  columnCount,
  lifecycleBars,
  lifecycleScale,
  type Cohort,
  type LifecyclePoint,
} from './retention';

describe('retentionRate', () => {
  it('is null for a period that has not elapsed — never 0', () => {
    // The bug: `users ?? 0` in a template renders a cohort that simply has not
    // aged yet as total churn.
    expect(retentionRate(null, 100)).toBeNull();
  });

  it('is null rather than 0/0 for an empty cohort', () => {
    expect(retentionRate(0, 0)).toBeNull();
  });

  it('is 0 — not null — when the period elapsed and nobody returned', () => {
    // The other half of the same distinction: a real zero must survive.
    expect(retentionRate(0, 100)).toBe(0);
  });

  it('computes a rate for an elapsed period', () => {
    expect(retentionRate(25, 100)).toBe(0.25);
  });
});

describe('cellKind', () => {
  it('renders period 0 as the cohort size, not 100%', () => {
    expect(cellKind(0, 10, 10)).toBe('size');
  });

  it('renders an unelapsed period as empty', () => {
    expect(cellKind(3, null, 10)).toBe('empty');
  });

  it('renders an elapsed zero as a rate, so real churn still shows', () => {
    expect(cellKind(3, 0, 10)).toBe('rate');
  });

  it('renders an empty cohort as empty rather than dividing by zero', () => {
    expect(cellKind(2, 0, 0)).toBe('empty');
  });
});

describe('rampStep', () => {
  it('separates a true zero from the lowest non-zero band', () => {
    // If these collided, "nobody came back" and "a few came back" would paint
    // identically.
    expect(rampStep(0)).toBe(0);
    expect(rampStep(0.05)).toBe(1);
  });

  it('climbs monotonically', () => {
    const steps = [0, 0.05, 0.2, 0.4, 0.9].map(rampStep);
    expect(steps).toEqual([...steps].sort((a, b) => a - b));
    expect(new Set(steps).size).toBe(5);
  });
});

describe('columnCount', () => {
  it('takes the widest cohort, not the first', () => {
    // A table sized from cohorts[0] truncates every later row when the first
    // cohort happens to be the shortest.
    const cohorts: Cohort[] = [
      { start: '2026-08-28', size: 1, periods: [1] },
      { start: '2026-08-27', size: 1, periods: [1, 1, 1] },
    ];
    expect(columnCount(cohorts)).toBe(3);
  });

  it('is 0 for no cohorts', () => {
    expect(columnCount([])).toBe(0);
  });
});

describe('lifecycleBars', () => {
  const points: LifecyclePoint[] = [
    {
      start: '2026-08-28',
      new_users: 5,
      returning_users: 3,
      resurrected_users: 1,
      dormant_users: 2,
    },
  ];

  it('draws dormant below the axis', () => {
    expect(lifecycleBars(points)[0].dormant).toBe(-2);
  });

  it('keeps the three positive series as the active total', () => {
    // The backend guarantees these partition the active set; if this sum ever
    // disagrees with the API's own active count, one of them double-counts.
    expect(lifecycleBars(points)[0].active).toBe(9);
  });

  it('preserves series order so the stack is stable across renders', () => {
    expect(lifecycleBars(points)[0].positive.map((s) => s.key)).toEqual([
      'new',
      'returning',
      'resurrected',
    ]);
  });
});

describe('lifecycleScale', () => {
  it('never returns 0, so bar heights cannot become NaN', () => {
    expect(lifecycleScale(lifecycleBars([]))).toBe(1);
    const flat: LifecyclePoint[] = [
      {
        start: '2026-08-28',
        new_users: 0,
        returning_users: 0,
        resurrected_users: 0,
        dormant_users: 0,
      },
    ];
    expect(lifecycleScale(lifecycleBars(flat))).toBe(1);
  });

  it('accounts for a dormant bar taller than any active bar', () => {
    const points: LifecyclePoint[] = [
      {
        start: '2026-08-28',
        new_users: 1,
        returning_users: 0,
        resurrected_users: 0,
        dormant_users: 40,
      },
    ];
    expect(lifecycleScale(lifecycleBars(points))).toBe(40);
  });
});

describe('gridToCsv', () => {
  const cohorts: Cohort[] = [
    { start: '2026-08-22', size: 10, periods: [10, 6, null] },
    { start: '2026-08-23', size: 4, periods: [4] },
  ];

  it('exports raw counts with a granularity-named header', () => {
    const lines = gridToCsv(cohorts, 'day').split('\n');
    expect(lines[0]).toBe('cohort,size,day_0_users,day_1_users,day_2_users');
    expect(lines[1]).toBe('2026-08-22,10,10,6,');
  });

  it('exports an unelapsed period as an EMPTY field, never 0', () => {
    // A blank is absent to =AVERAGE(); a 0 is data. Exporting 0 here would
    // poison every downstream aggregate with fake total churn.
    const lines = gridToCsv(cohorts, 'day').split('\n');
    expect(lines[1].endsWith(',')).toBe(true);
    expect(lines[1]).not.toMatch(/,0$/);
  });

  it('pads ragged cohorts to the widest row', () => {
    const lines = gridToCsv(cohorts, 'week').split('\n');
    expect(lines[2]).toBe('2026-08-23,4,4,,');
    expect(lines[0].split(',').length).toBe(lines[2].split(',').length);
  });
});

describe('cellLabel', () => {
  it('renders period 0 as 100% in rate mode, never as a duplicate of size', () => {
    // The Users column already shows the cohort size; rendering size AGAIN in
    // the first period column is the two-identical-numbers confusion the
    // 2026-08-29 screenshot feedback was about.
    expect(cellLabel('size', 'rate', null, 43367)).toBe('100%');
  });

  it('renders period 0 as the count in count mode', () => {
    expect(cellLabel('size', 'count', null, 43367)).toBe('43,367');
  });

  it('renders rate cells as a percentage or a count by mode', () => {
    expect(cellLabel('rate', 'rate', 1735, 43367)).toBe('4%');
    expect(cellLabel('rate', 'count', 1735, 43367)).toBe('1,735');
  });

  it('renders empty cells as empty in BOTH modes', () => {
    // An unelapsed period has no answer in any unit.
    expect(cellLabel('empty', 'rate', null, 10)).toBe('');
    expect(cellLabel('empty', 'count', null, 10)).toBe('');
  });
});

describe('lifecycle layout', () => {
  const bar = (active: number, dormant: number) =>
    ({
      start: '2026-08-20',
      positive: [{ key: 'new' as const, value: active }],
      dormant: -dormant,
      active,
    }) as import('./retention').LifecycleBar;

  it('rounds the axis top to a nice number', () => {
    expect(niceCeil(94858)).toBe(100000);
    expect(niceCeil(1735)).toBe(2000);
    expect(niceCeil(43)).toBe(50);
    expect(niceCeil(0)).toBe(1);
  });

  it('gives the whole plot to actives when nothing is dormant', () => {
    const l = lifecycleLayout([bar(100, 0)]);
    expect(l.posShare).toBe(1);
    expect(l.negShare).toBe(0);
    expect(l.negTop).toBe(0);
  });

  it('keeps a small dormant strip visible but proportional', () => {
    // 100k actives vs 2k dormant: proportionally ~2% — floored at 8% so the
    // strip is visible, which is the whole complaint the 50/50 split solved
    // by wasting half the chart.
    const l = lifecycleLayout([bar(94858, 1735)]);
    expect(l.negShare).toBe(0.08);
    expect(l.posShare).toBeCloseTo(0.92);
  });

  it('caps the dormant region at half even in a churn catastrophe', () => {
    const l = lifecycleLayout([bar(10, 100000)]);
    expect(l.negShare).toBe(0.5);
  });

  it('ticks the positive axis at 0, half, top', () => {
    expect(lifecycleLayout([bar(94858, 0)]).posTicks).toEqual([0, 50000, 100000]);
  });
});
