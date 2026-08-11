import { describe, expect, it } from 'vitest';
import {
  allSameIp,
  describeSession,
  hasCurrentSession,
  otherSessionCount,
} from './account-sessions';
import type { AccountSession } from './index';

function session(over: Partial<AccountSession> = {}): AccountSession {
  return {
    id: 'a',
    created_at: '2026-08-01T10:00:00Z',
    last_used_at: '2026-08-01T10:00:00Z',
    expires_at: '2026-08-31T10:00:00Z',
    current: false,
    user_agent: null,
    browser: null,
    os: null,
    device_kind: null,
    ip: null,
    revoked_at: null,
    revoked_reason: null,
    ...over,
  };
}

describe('describeSession', () => {
  it('prefers browser and os together', () => {
    expect(describeSession(session({ browser: 'Chrome', os: 'Mac OSX' }))).toBe('Chrome on Mac OSX');
  });

  it('falls back to whichever half it has', () => {
    expect(describeSession(session({ browser: 'Safari' }))).toBe('Safari');
    expect(describeSession(session({ os: 'Windows 11' }))).toBe('Windows 11');
  });

  it('falls back to a truncated raw user agent', () => {
    const raw = 'x'.repeat(80);
    const out = describeSession(session({ user_agent: raw }));
    expect(out).toHaveLength(61); // 60 characters plus the ellipsis
    expect(out.endsWith('…')).toBe(true);
    expect(describeSession(session({ user_agent: 'curl/8.5.0' }))).toBe('curl/8.5.0');
  });

  it('falls back to Unknown device when there is nothing at all', () => {
    expect(describeSession(session())).toBe('Unknown device');
    // The server normalises woothee's "UNKNOWN" sentinel to null, but a
    // whitespace-only string must not render as a blank cell either.
    expect(describeSession(session({ browser: '  ', os: '', user_agent: '   ' }))).toBe(
      'Unknown device',
    );
  });
});

// The `sortSessions` block that stood here is gone with the function — the
// table's ordering is covered by `account-session-sort.test.ts` now, and tests
// for a function nothing calls are coverage of nothing.

describe('otherSessionCount and hasCurrentSession', () => {
  it('are zero and false on an empty list', () => {
    expect(otherSessionCount([])).toBe(0);
    expect(hasCurrentSession([])).toBe(false);
  });

  it('counts only live, non-current rows', () => {
    const list = [
      session({ id: 'a', current: true }),
      session({ id: 'b' }),
      session({ id: 'c', revoked_at: '2026-08-01T10:30:00Z' }),
    ];
    expect(otherSessionCount(list)).toBe(1);
    expect(hasCurrentSession(list)).toBe(true);
  });

  it('reports no current session for a legacy token, which is what disables the UI', () => {
    const list = [session({ id: 'a' }), session({ id: 'b' })];
    expect(hasCurrentSession(list)).toBe(false);
    expect(otherSessionCount(list)).toBe(2);
  });
});

describe('allSameIp', () => {
  it('is true only when two or more live rows share one address', () => {
    expect(allSameIp([session({ ip: '10.0.0.5' }), session({ ip: '10.0.0.5' })])).toBe(true);
  });

  it('is false for mixed, single-row, empty and null-bearing lists', () => {
    expect(allSameIp([session({ ip: '10.0.0.5' }), session({ ip: '10.0.0.6' })])).toBe(false);
    expect(allSameIp([session({ ip: '10.0.0.5' })])).toBe(false);
    expect(allSameIp([])).toBe(false);
    expect(allSameIp([session({ ip: '10.0.0.5' }), session({ ip: null })])).toBe(false);
  });

  it('ignores revoked rows, which may legitimately come from anywhere', () => {
    const list = [
      session({ id: 'a', ip: '10.0.0.5' }),
      session({ id: 'b', ip: '10.0.0.5' }),
      session({ id: 'c', ip: '203.0.113.9', revoked_at: '2026-08-01T10:30:00Z' }),
    ];
    expect(allSameIp(list)).toBe(true);
  });
});
