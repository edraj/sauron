// Pinned before the first `Date` is constructed. Europe/London is chosen for
// one reason: it observes DST, and the machine this suite usually runs on
// (Asia/Baghdad, UTC+03) has not since 2008 — so without this the DST case
// below would pass by never encountering a DST transition at all.
// Cast rather than `@types/node`: this is the only file in the suite that
// needs `process`, and adding a dependency to type one assignment is a worse
// trade than one narrow cast.
(globalThis as unknown as { process: { env: Record<string, string> } }).process.env.TZ =
  'Europe/London';

import { describe, it, expect } from 'vitest';
import {
  toParams,
  fromParams,
  validate,
  describeFilter,
  defaultFilter,
  localInputToUtc,
  utcToLocalInput,
  type TimeField,
  type TimeFilterState,
} from './time-filter';

const FIELDS: TimeField[] = [
  { key: 'last_seen', label: 'Last seen' },
  { key: 'first_seen', label: 'First seen' },
];

describe('localInputToUtc', () => {
  it('reads a bare date as the START of that local day for `from`', () => {
    const iso = localInputToUtc('2026-08-01', 'from')!;
    // 1 Aug 2026 is BST (UTC+1), so local midnight is 23:00 the previous day UTC.
    expect(iso).toBe('2026-07-31T23:00:00.000Z');
  });

  it('reads a bare date as the FOLLOWING local midnight for `to`', () => {
    // The interval is half-open, so "between 1 Aug and 3 Aug" must run to the
    // start of 4 Aug to include all of 3 Aug. Truncating `to` to the start of
    // its own day silently drops the final day, which reads as a data bug
    // rather than a boundary convention.
    const iso = localInputToUtc('2026-08-03', 'to')!;
    expect(iso).toBe('2026-08-03T23:00:00.000Z');
  });

  it('spans exactly one CALENDAR day across a DST transition, not 24 hours', () => {
    // 29 March 2026 is the UK spring-forward date: the clocks go forward at
    // 01:00, making it a 23-hour day. Computing `to` as `from + 86_400_000`
    // would land an hour into 30 March and quietly widen the window.
    const from = localInputToUtc('2026-03-29', 'from')!;
    const to = localInputToUtc('2026-03-29', 'to')!;
    const hours = (new Date(to).getTime() - new Date(from).getTime()) / 3_600_000;
    expect(hours).toBe(23);
  });

  it('rolls over a month boundary', () => {
    const from = localInputToUtc('2026-01-31', 'from')!;
    const to = localInputToUtc('2026-01-31', 'to')!;
    expect(new Date(to).getTime() - new Date(from).getTime()).toBe(86_400_000);
    expect(to).toBe('2026-02-01T00:00:00.000Z');
  });

  it('takes an explicit time exactly, for either bound', () => {
    // An explicit time is what the user meant; only a BARE date needs a
    // convention applied to it.
    expect(localInputToUtc('2026-08-01T14:20', 'from')).toBe('2026-08-01T13:20:00.000Z');
    expect(localInputToUtc('2026-08-01T14:20', 'to')).toBe('2026-08-01T13:20:00.000Z');
  });

  it('rejects unparseable input rather than inventing an instant', () => {
    expect(localInputToUtc('', 'from')).toBeNull();
    expect(localInputToUtc('not-a-date', 'from')).toBeNull();
    expect(localInputToUtc('2026-13-45', 'from')).toBeNull();
  });

  it('round-trips through utcToLocalInput', () => {
    const iso = localInputToUtc('2026-08-01T14:20', 'from')!;
    expect(utcToLocalInput(iso)).toBe('2026-08-01T14:20');
  });
});

describe('toParams', () => {
  it('sends since_days for `last`, and no bounds', () => {
    const p = toParams({ field: 'last_seen', mode: 'last', lastDays: 7 }, 'last_seen');
    expect(p.get('since_days')).toBe('7');
    expect(p.get('from')).toBeNull();
    expect(p.get('to')).toBeNull();
  });

  it('omits time_field when it is the page default', () => {
    // Keeps the URL and the wire free of a parameter that says nothing, so a
    // shared link of an untouched page looks untouched.
    const p = toParams({ field: 'last_seen', mode: 'last', lastDays: 7 }, 'last_seen');
    expect(p.get('time_field')).toBeNull();
  });

  it('sends time_field when it is not the default', () => {
    const p = toParams({ field: 'first_seen', mode: 'last', lastDays: 7 }, 'last_seen');
    expect(p.get('time_field')).toBe('first_seen');
  });

  it('never sends since_days alongside an explicit bound', () => {
    // The server ignores since_days when a bound is present. Sending it anyway
    // would put a request on the wire that reads as two conflicting windows.
    const p = toParams({ field: 'first_seen', mode: 'after', from: '2026-08-01T00:00:00.000Z' }, 'last_seen');
    expect(p.get('from')).toBe('2026-08-01T00:00:00.000Z');
    expect(p.get('since_days')).toBeNull();
    expect(p.get('to')).toBeNull();
  });

  it('sends only `to` for before', () => {
    const p = toParams({ field: 'last_seen', mode: 'before', to: '2026-08-01T00:00:00.000Z' }, 'last_seen');
    expect(p.get('to')).toBe('2026-08-01T00:00:00.000Z');
    expect(p.get('from')).toBeNull();
  });

  it('sends both bounds for between', () => {
    const p = toParams(
      { field: 'last_seen', mode: 'between', from: '2026-08-01T00:00:00.000Z', to: '2026-08-05T00:00:00.000Z' },
      'last_seen',
    );
    expect(p.get('from')).toBe('2026-08-01T00:00:00.000Z');
    expect(p.get('to')).toBe('2026-08-05T00:00:00.000Z');
  });
});

describe('fromParams', () => {
  it('falls back to the page default when nothing is present', () => {
    const tf = fromParams(new URLSearchParams(''), FIELDS, 'last_seen', 30);
    expect(tf).toEqual({ field: 'last_seen', mode: 'last', lastDays: 30 });
  });

  it('drops a time_field the page does not offer', () => {
    // A stale or hand-edited link must degrade to a valid view rather than
    // producing a 400 on first paint.
    const tf = fromParams(new URLSearchParams('time_field=occurred_at'), FIELDS, 'last_seen', 30);
    expect(tf.field).toBe('last_seen');
  });

  it('drops an inverted range rather than sending a request that 400s', () => {
    const tf = fromParams(
      new URLSearchParams('from=2026-08-05T00:00:00.000Z&to=2026-08-01T00:00:00.000Z'),
      FIELDS,
      'last_seen',
      30,
    );
    expect(tf.mode).toBe('last');
  });

  it('ignores a non-numeric since_days', () => {
    const tf = fromParams(new URLSearchParams('since_days=abc'), FIELDS, 'last_seen', 30);
    expect(tf.lastDays).toBe(30);
  });

  it('infers the mode from which bounds are present', () => {
    const after = fromParams(new URLSearchParams('from=2026-08-01T00:00:00.000Z'), FIELDS, 'last_seen', 30);
    expect(after.mode).toBe('after');
    const before = fromParams(new URLSearchParams('to=2026-08-01T00:00:00.000Z'), FIELDS, 'last_seen', 30);
    expect(before.mode).toBe('before');
    const between = fromParams(
      new URLSearchParams('from=2026-08-01T00:00:00.000Z&to=2026-08-05T00:00:00.000Z'),
      FIELDS,
      'last_seen',
      30,
    );
    expect(between.mode).toBe('between');
  });

  it('round-trips every mode through toParams', () => {
    const cases: TimeFilterState[] = [
      { field: 'last_seen', mode: 'last', lastDays: 30 },
      { field: 'first_seen', mode: 'last', lastDays: 365 },
      { field: 'first_seen', mode: 'after', from: '2026-08-01T00:00:00.000Z' },
      { field: 'first_seen', mode: 'before', to: '2026-08-01T00:00:00.000Z' },
      {
        field: 'last_seen',
        mode: 'between',
        from: '2026-08-01T00:00:00.000Z',
        to: '2026-08-05T00:00:00.000Z',
      },
    ];
    for (const tf of cases) {
      expect(fromParams(toParams(tf, 'last_seen'), FIELDS, 'last_seen', 30)).toEqual(tf);
    }
  });
});

describe('validate', () => {
  it('accepts each well-formed mode', () => {
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: 30 })).toBeNull();
    expect(validate({ field: 'last_seen', mode: 'after', from: '2026-08-01T00:00:00.000Z' })).toBeNull();
    expect(validate({ field: 'last_seen', mode: 'before', to: '2026-08-01T00:00:00.000Z' })).toBeNull();
    expect(
      validate({
        field: 'last_seen',
        mode: 'between',
        from: '2026-08-01T00:00:00.000Z',
        to: '2026-08-05T00:00:00.000Z',
      }),
    ).toBeNull();
  });

  it('rejects an inverted range', () => {
    const msg = validate({
      field: 'last_seen',
      mode: 'between',
      from: '2026-08-05T00:00:00.000Z',
      to: '2026-08-01T00:00:00.000Z',
    });
    expect(msg).toMatch(/earlier/i);
  });

  it('rejects an equal-bound range, since the half-open interval is empty', () => {
    expect(
      validate({
        field: 'last_seen',
        mode: 'between',
        from: '2026-08-01T00:00:00.000Z',
        to: '2026-08-01T00:00:00.000Z',
      }),
    ).toMatch(/earlier/i);
  });

  it('rejects a between missing either bound', () => {
    expect(validate({ field: 'last_seen', mode: 'between', from: '2026-08-05T00:00:00.000Z' })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'between', to: '2026-08-05T00:00:00.000Z' })).toBeTruthy();
  });

  it('rejects an after/before missing its bound', () => {
    expect(validate({ field: 'last_seen', mode: 'after' })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'before' })).toBeTruthy();
  });

  it('rejects a day count below 1 or above the 365 ceiling', () => {
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: 0 })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: -5 })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: 366 })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: 365 })).toBeNull();
  });

  it('rejects a non-integer day count', () => {
    // A `bind:value` on a raw input is typed `any` by svelte, so a number or a
    // fractional value can reach here despite the `number` annotation.
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: 3.5 })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: NaN })).toBeTruthy();
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: '30' as unknown as number })).toBeTruthy();
  });
});

describe('describeFilter', () => {
  it('names the field, not just the window', () => {
    expect(describeFilter({ field: 'first_seen', mode: 'last', lastDays: 7 }, FIELDS)).toBe(
      'First seen in the last 7 days',
    );
  });

  it('says 24 hours rather than 1 days', () => {
    expect(describeFilter({ field: 'last_seen', mode: 'last', lastDays: 1 }, FIELDS)).toBe(
      'Last seen in the last 24 hours',
    );
  });

  it('falls back to the raw key for a field it does not know', () => {
    expect(describeFilter({ field: 'mystery', mode: 'last', lastDays: 7 }, FIELDS)).toBe(
      'mystery in the last 7 days',
    );
  });
});

describe('defaultFilter', () => {
  it('builds the `last` mode at the given day count', () => {
    expect(defaultFilter('last_seen', 30)).toEqual({ field: 'last_seen', mode: 'last', lastDays: 30 });
  });
});
