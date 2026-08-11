import { describe, expect, it, vi } from 'vitest';
import { OPERATION_DEFAULT_SORT, operationAccessor } from './performance-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { PerfSummaryRow } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order. The four latencies in
 * particular are all distinct constants, so p95's accessor cannot pass p99's
 * test.
 */
function row(over: Partial<PerfSummaryRow> & { name: string }): PerfSummaryRow {
  return {
    op: 'http',
    count: 40,
    p50: 11,
    p75: 22,
    p95: 33,
    p99: 44,
    avg: 55,
    error_rate: 0.02,
    ...over,
  };
}

const order = (rows: PerfSummaryRow[], key: string, dir: SortDir): string[] =>
  sortRows(rows, operationAccessor(key), dir).map((r) => r.name);

describe('operationAccessor', () => {
  it('orders Throughput by the count, not the thousands-separated text', () => {
    // The cell renders `toLocaleString()`: "1,000,052" sorts BEFORE "48" as
    // text. Every other field ties, so a mis-wired accessor collapses to input
    // order — which is not the expected order either way.
    const rows = [
      row({ name: 'mid', count: 700 }),
      row({ name: 'busy', count: 1_000_052 }),
      row({ name: 'rare', count: 48 }),
    ];
    expect(order(rows, 'throughput', 'desc')).toEqual(['busy', 'mid', 'rare']);
    expect(order(rows, 'throughput', 'asc')).toEqual(['rare', 'mid', 'busy']);
  });

  it('orders each latency column by ITS OWN milliseconds', () => {
    // FOUR rows, not three, and that is the point rather than an accident.
    //
    // This table has eight accessors and every one of them is a plausible
    // mis-wiring of every other — the four percentiles are four
    // near-identical lines in the map. With three rows there are only six
    // possible orderings, so eight accessors CANNOT all differ and at least
    // two mis-wirings survive by arithmetic alone. An earlier version of this
    // test had exactly that hole: p50 (300/100/200) and p99 (900/200/500)
    // descend in the same order, so swapping those two accessors passed the
    // whole file while the comment above claimed each column was unique.
    //
    // Four rows give 24 orderings, and these values were chosen so that all
    // EIGHT accessors — name, op, throughput, p50, p95, p99, avg, error_rate —
    // produce a distinct order WITHIN each direction: eight distinct
    // descending orders, and separately eight distinct ascending ones. Not all
    // sixteen differ from each other — `p95` ascending happens to equal `p99`
    // descending, and vice versa — but that is a cross-DIRECTION coincidence
    // and no substitution can reach it, because swapping one accessor for
    // another changes the field while the assertion fixes the direction.
    //
    // An accessor that ties every row is excluded too (a constant, or the
    // unrendered `p75`): the input order n3,n1,n4,n2 is none of the expected
    // orders below.
    //
    // The counting argument for four rows holds under exactly this discipline
    // — no deliberate ties, so a column's ascending order determines its
    // descending one and eight accessors really are competing for six
    // three-row permutations. A fixture that tied values on purpose could
    // squeeze more signatures out of three rows; it would also reintroduce
    // the tie-collapse hole, which is the opposite of what this is for.
    //
    // The row LABEL is `name`, which is itself a real accessor target, so the
    // labels are deliberately anti-correlated with everything: name ascending
    // is n1,n2,n3,n4 and no assertion here expects that.
    const rows = [
      row({ name: 'n3', op: 'custom', count: 10, p50: 40, p95: 5, p99: 900, avg: 7, error_rate: 0.001 }),
      row({ name: 'n1', op: 'http', count: 40, p50: 10, p95: 900, p99: 5, avg: 90, error_rate: 0.02 }),
      row({ name: 'n4', op: 'navigation', count: 20, p50: 90, p95: 40, p99: 20, avg: 400, error_rate: 0.5 }),
      row({ name: 'n2', op: 'resource', count: 5, p50: 95, p95: 20, p99: 40, avg: 5, error_rate: 0.1 }),
    ];
    expect(order(rows, 'p50', 'desc')).toEqual(['n2', 'n4', 'n3', 'n1']);
    expect(order(rows, 'p50', 'asc')).toEqual(['n1', 'n3', 'n4', 'n2']);
    // "900 ms" vs "40 ms" vs "20 ms" vs "5 ms": ordering LatencyBadge's text
    // descending would not give this.
    expect(order(rows, 'p95', 'desc')).toEqual(['n1', 'n4', 'n2', 'n3']);
    expect(order(rows, 'p95', 'asc')).toEqual(['n3', 'n2', 'n4', 'n1']);
    expect(order(rows, 'p99', 'desc')).toEqual(['n3', 'n2', 'n4', 'n1']);
    expect(order(rows, 'p99', 'asc')).toEqual(['n1', 'n4', 'n2', 'n3']);
    expect(order(rows, 'avg', 'desc')).toEqual(['n4', 'n1', 'n3', 'n2']);
    expect(order(rows, 'avg', 'asc')).toEqual(['n2', 'n3', 'n1', 'n4']);
  });

  it('orders Error rate by the ratio, not by formatPercent text', () => {
    // `formatPercent` renders these as "10.0%", "2.0%" and "0.5%"; as text
    // ascending that is "0.5%", "10.0%", "2.0%" — the ten-percent row lands
    // in the middle. Throughput ties across all three.
    const rows = [
      row({ name: 'two', error_rate: 0.02 }),
      row({ name: 'ten', error_rate: 0.1 }),
      row({ name: 'half', error_rate: 0.005 }),
    ];
    expect(order(rows, 'error_rate', 'desc')).toEqual(['ten', 'two', 'half']);
    expect(order(rows, 'error_rate', 'asc')).toEqual(['half', 'two', 'ten']);
  });

  it('orders Name and Op by their own fields', () => {
    // Name order and Op order disagree, so neither can stand in for the other.
    const rows = [
      row({ name: 'a-load', op: 'screen_load' }),
      row({ name: 'z-fetch', op: 'http' }),
      row({ name: 'm-nav', op: 'navigation' }),
    ];
    expect(order(rows, 'name', 'asc')).toEqual(['a-load', 'm-nav', 'z-fetch']);
    expect(order(rows, 'op', 'asc')).toEqual(['z-fetch', 'm-nav', 'a-load']);
  });

  it('orders Op the same way its rendered label would', () => {
    // The claim `performance-sort.ts` makes for sorting the raw `op` instead
    // of the page's `opLabel`, checked rather than asserted: `opLabel` only
    // swaps an underscore for a space, so over the five values the page's OPS
    // filter offers, label order and raw order are the same list. If a sixth
    // op ever breaks that, this fails and the accessor has to take the label.
    const opLabel = (o: string) => o.replace('_', ' ');
    const ops = ['navigation', 'http', 'screen_load', 'resource', 'custom'];
    const rows = ops.map((op) => row({ name: op, op }));
    const byLabel = [...ops].sort((x, y) => opLabel(x).localeCompare(opLabel(y)));
    expect(order(rows, 'op', 'asc')).toEqual(byLabel);
  });

  it('falls back to Throughput for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Two independent things have to be false here, and each needs its own
    // property of this fixture:
    //
    //  - a fallback to NAME — the row label, and the likeliest wrong fallback
    //    in a file whose rows are labelled by name — is excluded because name
    //    order runs opposite to count order, `aaa` being the BUSY one;
    //  - a fallback that TIES every row is excluded because the rows are
    //    supplied in the OPPOSITE order to the expected one. Six of this map's
    //    eight accessors (`op`, `p50`, `p95`, `p99`, `avg`, `error_rate`) hold
    //    their constant default across these two rows, so each collapses to
    //    input order — and while input order WAS the expected order this
    //    assertion passed for all six no matter what the fallback did.
    const rows = [
      row({ name: 'zzz', count: 3 }),
      row({ name: 'aaa', count: 900 }),
    ];
    expect(order(rows, 'no-such-column', 'desc')).toEqual(['aaa', 'zzz']);
    expect(OPERATION_DEFAULT_SORT).toEqual({ key: 'throughput', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order([row({ name: 'a' })], 'error_rate', 'desc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
