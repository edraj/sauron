import { describe, expect, it } from 'vitest';
import { rankOf, sortRows } from './sort-rows';

const names = (rows: { name: string }[]) => rows.map((r) => r.name);

describe('sortRows', () => {
  it('orders numbers by value, not lexically', () => {
    const rows = [{ n: 9 }, { n: 100 }, { n: 10 }];
    expect(sortRows(rows, (r) => r.n, 'asc').map((r) => r.n)).toEqual([9, 10, 100]);
    expect(sortRows(rows, (r) => r.n, 'desc').map((r) => r.n)).toEqual([100, 10, 9]);
  });

  it('orders dates chronologically', () => {
    const rows = [
      { name: 'b', at: new Date('2026-02-01T00:00:00Z') },
      { name: 'a', at: new Date('2026-01-01T00:00:00Z') },
    ];
    expect(names(sortRows(rows, (r) => r.at, 'asc'))).toEqual(['a', 'b']);
  });

  it('orders ISO timestamp strings chronologically', () => {
    // Rows carry timestamps as ISO strings straight off the wire far more often
    // than as Date objects, and ISO-8601 sorts correctly as text — but only if
    // it is not run through a locale collator, which reorders punctuation.
    const rows = [
      { name: 'b', at: '2026-02-01T00:00:00Z' },
      { name: 'a', at: '2026-01-01T00:00:00Z' },
    ];
    expect(names(sortRows(rows, (r) => r.at, 'asc'))).toEqual(['a', 'b']);
  });

  it('orders text case-insensitively and in locale order', () => {
    const rows = [{ name: 'banana' }, { name: 'Apple' }, { name: 'cherry' }];
    expect(names(sortRows(rows, (r) => r.name, 'asc'))).toEqual([
      'Apple',
      'banana',
      'cherry',
    ]);
  });

  it('puts nulls last in BOTH directions', () => {
    // A null is an absent value, not a small one. Sorting it as -Infinity puts
    // "never seen" at the top of a "least recently seen" sort, which reads as a
    // real answer and is not one.
    const rows = [{ name: 'a', n: 2 }, { name: 'gap', n: null }, { name: 'b', n: 1 }];
    expect(names(sortRows(rows, (r) => r.n, 'asc'))).toEqual(['b', 'a', 'gap']);
    expect(names(sortRows(rows, (r) => r.n, 'desc'))).toEqual(['a', 'b', 'gap']);
  });

  it('is stable — equal keys keep their input order', () => {
    const rows = [
      { name: 'first', n: 1 },
      { name: 'second', n: 1 },
      { name: 'third', n: 1 },
    ];
    expect(names(sortRows(rows, (r) => r.n, 'desc'))).toEqual([
      'first',
      'second',
      'third',
    ]);
  });

  it('does not mutate the input', () => {
    const rows = [{ n: 2 }, { n: 1 }];
    sortRows(rows, (r) => r.n, 'asc');
    expect(rows.map((r) => r.n)).toEqual([2, 1]);
  });

  it('sorts booleans false-before-true ascending — "unchecked" first', () => {
    // No cast on the accessor: if `boolean` were missing from `SortValue`,
    // `(r) => r.enabled` would fail to type-check against
    // `(row: T) => SortValue` (caught by `npm run check`, not by this runtime
    // assertion — a boolean-only column's order is the same either way, since
    // `compare`'s text fallback happens to sort the strings "false"/"true" in
    // the same order as `Number(false)`/`Number(true)`). Monitors' and Alert
    // rules' `enabled` columns are exactly this shape.
    const rows = [{ name: 'on', enabled: true }, { name: 'off', enabled: false }];
    expect(names(sortRows(rows, (r) => r.enabled, 'asc'))).toEqual(['off', 'on']);
    expect(names(sortRows(rows, (r) => r.enabled, 'desc'))).toEqual(['on', 'off']);
  });
});

describe('rankOf', () => {
  // Least severe first, so a higher rank is worse — the direction `rankOf`
  // documents and every caller inherits.
  const severity = rankOf(['debug', 'info', 'warning', 'error', 'fatal']);
  const levels = (rows: { l: string }[]) => rows.map((r) => r.l);

  it('orders by rank, not alphabetically, in both directions', () => {
    // Every one of the four possible answers here is DIFFERENT, which is the
    // point of this fixture — a rank test whose levels happen to be in
    // alphabetical order cannot tell a ranking from a spelling:
    //   text asc  → debug, fatal, warning   (fatal above warning: wrong)
    //   text desc → warning, fatal, debug   (warning the most severe: wrong)
    //   rank asc  → debug, warning, fatal
    //   rank desc → fatal, warning, debug
    // So the old text accessor fails this assertion in BOTH directions rather
    // than coincidentally agreeing with it in one.
    const rows = [{ l: 'debug' }, { l: 'fatal' }, { l: 'warning' }];
    expect(levels(sortRows(rows, (r) => severity(r.l), 'asc'))).toEqual([
      'debug',
      'warning',
      'fatal',
    ]);
    expect(levels(sortRows(rows, (r) => severity(r.l), 'desc'))).toEqual([
      'fatal',
      'warning',
      'debug',
    ]);
  });

  it('sorts an unranked value LAST in both directions, not first in either', () => {
    // A level the ladder has never heard of — a status the backend added and
    // this build does not know — must not lead either list. It is unknown, not
    // extreme.
    //
    // This is where `order.length` (the obvious alternative to null) dies: it
    // is last ascending and FIRST descending, so `wat` would head the
    // worst-first list. The text accessor dies here too — as text `wat` sorts
    // after both real levels ascending and before both descending.
    const rows = [{ l: 'fatal' }, { l: 'wat' }, { l: 'info' }];
    expect(levels(sortRows(rows, (r) => severity(r.l), 'asc'))).toEqual([
      'info',
      'fatal',
      'wat',
    ]);
    expect(levels(sortRows(rows, (r) => severity(r.l), 'desc'))).toEqual([
      'fatal',
      'info',
      'wat',
    ]);
  });

  it('ranks the least severe value 0, and 0 is a rank rather than an absence', () => {
    // `ranks.get(value) || null` instead of `?? null` turns the bottom of every
    // ladder into an unknown, so `debug` would sort last with `wat`. The rank
    // assertions above cannot see that — `debug` is already last-but-one there.
    expect(severity('debug')).toBe(0);
    expect(severity('fatal')).toBe(4);
    const rows = [{ l: 'wat' }, { l: 'debug' }];
    expect(levels(sortRows(rows, (r) => severity(r.l), 'asc'))).toEqual(['debug', 'wat']);
    expect(levels(sortRows(rows, (r) => severity(r.l), 'desc'))).toEqual(['debug', 'wat']);
  });

  it('ranks null and undefined as absent, the same as an unknown value', () => {
    expect(severity(null)).toBeNull();
    expect(severity(undefined)).toBeNull();
    expect(severity('nope')).toBeNull();
  });
});
