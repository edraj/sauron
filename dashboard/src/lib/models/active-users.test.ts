import { describe, expect, it } from 'vitest';
import {
  decodeSelection,
  defaultWindow,
  describeSelection,
  encodeSelection,
  MAX_SELECTED_APPS,
  selectionCount,
  utcDayLabel,
  validateSelection,
  type AppEnvSelection,
} from './active-users';

describe('encodeSelection / decodeSelection', () => {
  it('sorts by app id so the URL and the server cache key are stable', () => {
    const sel: AppEnvSelection = { 'b-app': 'all', 'a-app': 'env-1' };
    expect(encodeSelection(sel)).toEqual(['a-app:env-1', 'b-app']);
  });

  it('emits a bare app id for "all" and round-trips it back', () => {
    const sel: AppEnvSelection = { 'a-app': 'all', 'b-app': 'none', 'c-app': 'env-9' };
    const encoded = encodeSelection(sel);
    expect(encoded).toEqual(['a-app', 'b-app:none', 'c-app:env-9']);
    expect(decodeSelection(encoded)).toEqual(sel);
  });

  it('decodes a bare app id as "all"', () => {
    expect(decodeSelection(['x'])).toEqual({ x: 'all' });
  });

  it('ignores an empty token rather than minting an empty app id', () => {
    expect(decodeSelection(['', 'x'])).toEqual({ x: 'all' });
  });
});

describe('selectionCount / validateSelection', () => {
  it('counts apps, not tokens', () => {
    expect(selectionCount({ a: 'all', b: 'none' })).toBe(2);
    expect(selectionCount({})).toBe(0);
  });

  it('rejects an empty selection', () => {
    const r = validateSelection({});
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toMatch(/at least one/i);
  });

  it('rejects more than MAX_SELECTED_APPS', () => {
    const sel: AppEnvSelection = {};
    for (let i = 0; i <= MAX_SELECTED_APPS; i += 1) sel[`app-${i}`] = 'all';
    const r = validateSelection(sel);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain(String(MAX_SELECTED_APPS));
  });

  it('accepts a selection at the cap', () => {
    const sel: AppEnvSelection = {};
    for (let i = 0; i < MAX_SELECTED_APPS; i += 1) sel[`app-${i}`] = 'all';
    expect(validateSelection(sel)).toEqual({ ok: true });
  });
});

describe('describeSelection', () => {
  const name = (id: string) => id.toUpperCase();
  const env = (_appId: string, choice: string) => (choice === 'all' ? 'All environments' : choice);

  it('names the environment when exactly one app is selected', () => {
    expect(describeSelection({ web: 'prod' }, name, env)).toBe('WEB · prod');
  });

  it('lists both when two are selected', () => {
    expect(describeSelection({ web: 'all', api: 'all' }, name, env)).toBe('API, WEB');
  });

  it('summarises the tail past two', () => {
    expect(describeSelection({ a: 'all', b: 'all', c: 'all', d: 'all' }, name, env)).toBe(
      'A, B +2 more',
    );
  });

  it('says so when nothing is selected', () => {
    expect(describeSelection({}, name, env)).toBe('No apps selected');
  });
});

describe('defaultWindow', () => {
  it('ends at the start of tomorrow UTC so today is included but never partial-labelled', () => {
    const now = new Date('2026-05-07T18:30:00Z');
    expect(defaultWindow(30, now)).toEqual({
      from: '2026-04-08T00:00:00.000Z',
      to: '2026-05-08T00:00:00.000Z',
    });
  });

  it('is unaffected by the viewer local zone', () => {
    const now = new Date('2026-05-07T23:59:59Z');
    expect(defaultWindow(7, now).to).toBe('2026-05-08T00:00:00.000Z');
  });
});

describe('utcDayLabel', () => {
  it('labels a UTC calendar day in UTC, not in the viewer local zone', () => {
    // The trap this exists for, pinned explicitly so the assertion below is
    // meaningful even on a UTC runner: `new Date('2026-07-31')` parses as UTC
    // but RENDERS in local time, so at a negative offset the bar for the 31st
    // is labelled "Jul 30" while the CSV row says 2026-07-31.
    const naive = new Date('2026-07-31').toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      timeZone: 'America/New_York',
    });
    expect(naive).toBe('Jul 30');
    expect(utcDayLabel('2026-07-31', 'en-US')).toBe('Jul 31');
  });

  it('passes a value it cannot parse straight through', () => {
    expect(utcDayLabel('not-a-day', 'en-US')).toBe('not-a-day');
  });
});
