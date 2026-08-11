import { describe, expect, it, vi } from 'vitest';
import { SESSION_DEFAULT_SORT, sessionAccessor } from './account-session-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { AccountSession } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor
 * reading a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 */
function sess(over: Partial<AccountSession> & { id: string }): AccountSession {
  return {
    created_at: '2026-04-01T08:00:00Z',
    last_used_at: '2026-04-01T08:00:00Z',
    expires_at: '2026-05-01T08:00:00Z',
    current: false,
    user_agent: 'constant/1.0',
    browser: 'Firefox',
    os: 'Fedora',
    device_kind: 'pc',
    ip: '10.0.0.1',
    revoked_at: null,
    revoked_reason: null,
    ...over,
  };
}

const order = (rows: AccountSession[], key: string, dir: SortDir): string[] =>
  sortRows(rows, sessionAccessor(key), dir).map((s) => s.id);

describe('sessionAccessor', () => {
  it('orders Device by the rendered phrase, not by the raw user agent or the id', () => {
    // Every row carries the SAME `user_agent`, so an accessor reading it calls
    // the three equal and leaves input order — which is not the expected
    // order. `describeSession` builds "Safari on iOS" etc. from browser/os.
    //
    // The id prefixes are what make this test able to fail. Labelled `safari` /
    // `chrome` / `firefox`, the ids collated in the same order as the phrases
    // they name, so `device: (s) => s.id` — ordering the column by a uuid the
    // user never sees — passed both assertions. `a-` / `m-` / `z-` put the ids
    // in an order the phrases do not share; keep it that way.
    const rows = [
      sess({ id: 'a-safari', browser: 'Safari', os: 'iOS' }),
      sess({ id: 'm-chrome', browser: 'Chrome', os: 'Android' }),
      sess({ id: 'z-firefox', browser: 'Firefox', os: 'Fedora' }),
    ];
    expect(order(rows, 'device', 'asc')).toEqual(['m-chrome', 'z-firefox', 'a-safari']);
    expect(order(rows, 'device', 'desc')).toEqual(['a-safari', 'z-firefox', 'm-chrome']);
  });

  it('orders IP numerically within an octet and keeps an unknown address last', () => {
    // 10.0.0.9 before 10.0.0.10 is only true because the shared collator runs
    // with `numeric: true`; plain lexical text puts .10 first. And a null IP is
    // absent, not lowest, so it stays last in BOTH directions.
    const rows = [
      sess({ id: 'ten', ip: '10.0.0.10' }),
      sess({ id: 'none', ip: null }),
      sess({ id: 'nine', ip: '10.0.0.9' }),
    ];
    expect(order(rows, 'ip', 'asc')).toEqual(['nine', 'ten', 'none']);
    expect(order(rows, 'ip', 'desc')).toEqual(['ten', 'nine', 'none']);
  });

  it('orders Signed in by created_at, independently of last use', () => {
    // `last_used_at` runs OPPOSITE to `created_at`, so the neighbouring
    // time column cannot satisfy this assertion.
    const rows = [
      sess({ id: 'old', created_at: '2026-04-01T08:00:00Z', last_used_at: '2026-04-09T08:00:00Z' }),
      sess({ id: 'new', created_at: '2026-04-07T08:00:00Z', last_used_at: '2026-04-02T08:00:00Z' }),
    ];
    expect(order(rows, 'signed_in', 'desc')).toEqual(['new', 'old']);
    expect(order(rows, 'signed_in', 'asc')).toEqual(['old', 'new']);
  });

  it('orders Last used by the instant each row displays — revoked_at for a revoked row', () => {
    // The revoked row's cell reads "Signed out <revoked_at>", so that is what
    // this column has to order it by. An accessor spelled `s.last_used_at`
    // would place `signedout` FIRST descending (its last_used_at is the most
    // recent of the three) while displaying the oldest visible timestamp of
    // the three — ordering by one number and showing another.
    const rows = [
      sess({
        id: 'signedout',
        last_used_at: '2026-04-20T08:00:00Z',
        revoked_at: '2026-04-02T08:00:00Z',
      }),
      sess({ id: 'live-old', last_used_at: '2026-04-05T08:00:00Z' }),
      sess({ id: 'live-new', last_used_at: '2026-04-11T08:00:00Z' }),
    ];
    expect(order(rows, 'last_used', 'desc')).toEqual(['live-new', 'live-old', 'signedout']);
    expect(order(rows, 'last_used', 'asc')).toEqual(['signedout', 'live-old', 'live-new']);
  });

  it('falls back to Last used for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // `created_at` runs opposite to `last_used_at`, so a fallback to the
    // Signed-in column instead would invert this.
    const rows = [
      sess({ id: 'a', created_at: '2026-04-09T08:00:00Z', last_used_at: '2026-04-01T08:00:00Z' }),
      sess({ id: 'b', created_at: '2026-04-01T08:00:00Z', last_used_at: '2026-04-09T08:00:00Z' }),
    ];
    expect(order(rows, 'no-such-column', 'desc')).toEqual(['b', 'a']);
    expect(SESSION_DEFAULT_SORT).toEqual({ key: 'last_used', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order([sess({ id: 'a' })], 'device', 'asc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
