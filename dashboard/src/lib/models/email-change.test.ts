import { describe, expect, it } from 'vitest';
import {
  canRequestEmailChange,
  canWithdrawEmailChange,
  emailChangePending,
  newEmailProblem,
  readEmailChangeToken,
} from './email-change';
import type { Member } from './index';

function member(over: Partial<Member> = {}): Member {
  return {
    user_id: 'u1',
    email: 'old@example.com',
    name: 'Old Name',
    is_active: true,
    credentials_invalidated_at: null,
    pending_email_change: null,
    grants: [],
    ...over,
  };
}

const PENDING = { new_email: 'new@example.com', expires_at: '2026-09-08T10:00:00Z' };

describe('readEmailChangeToken', () => {
  it('reads the token from a fragment query string', () => {
    expect(readEmailChangeToken('token=abc123')).toBe('abc123');
  });

  it('returns null for a bare page with no query at all', () => {
    // svelte-spa-router types `querystring` as `string | undefined`, and it is
    // genuinely undefined for `#/confirm-email-change` with nothing after it.
    expect(readEmailChangeToken(null)).toBeNull();
  });

  it('returns null for a query carrying no token', () => {
    expect(readEmailChangeToken('foo=bar')).toBeNull();
  });

  it('returns null for an empty or whitespace token', () => {
    expect(readEmailChangeToken('token=')).toBeNull();
    expect(readEmailChangeToken('token=%20%20')).toBeNull();
  });
});

describe('emailChangePending', () => {
  it('is false when nothing is pending', () => {
    expect(emailChangePending(member())).toBe(false);
  });

  it('is true while a change awaits confirmation', () => {
    expect(emailChangePending(member({ pending_email_change: PENDING }))).toBe(true);
  });

  it('is false when an older server omits the field entirely', () => {
    const legacy = member();
    delete (legacy as Partial<Member>).pending_email_change;
    expect(emailChangePending(legacy)).toBe(false);
  });
});

describe('canRequestEmailChange', () => {
  it('needs the credential permission', () => {
    expect(canRequestEmailChange(member(), 'someone-else', false)).toBe(false);
  });

  it('refuses self, mirroring the server 409', () => {
    expect(canRequestEmailChange(member(), 'u1', true)).toBe(false);
  });

  it('refuses a deactivated member, mirroring the server 409', () => {
    expect(canRequestEmailChange(member({ is_active: false }), 'other', true)).toBe(false);
  });

  it('refuses while one is already pending — withdraw is the other action', () => {
    expect(
      canRequestEmailChange(member({ pending_email_change: PENDING }), 'other', true),
    ).toBe(false);
  });

  it('allows the ordinary case', () => {
    expect(canRequestEmailChange(member(), 'other', true)).toBe(true);
  });
});

describe('canWithdrawEmailChange', () => {
  it('is the exact complement while pending, so a row never offers both', () => {
    const pending = member({ pending_email_change: PENDING });
    expect(canWithdrawEmailChange(pending, 'other', true)).toBe(true);
    expect(canRequestEmailChange(pending, 'other', true)).toBe(false);

    const idle = member();
    expect(canWithdrawEmailChange(idle, 'other', true)).toBe(false);
    expect(canRequestEmailChange(idle, 'other', true)).toBe(true);
  });

  it('needs the credential permission', () => {
    expect(
      canWithdrawEmailChange(member({ pending_email_change: PENDING }), 'other', false),
    ).toBe(false);
  });
});

describe('newEmailProblem', () => {
  // Mirrors the server's three refusals so the dialog never submits something
  // that comes back a 400 or a 409.
  it('rejects an empty address', () => {
    expect(newEmailProblem('', 'old@example.com')).toBe('empty');
  });

  it('rejects an address with no @', () => {
    expect(newEmailProblem('not-an-address', 'old@example.com')).toBe('malformed');
  });

  it('rejects the address the member already has, case-insensitively', () => {
    // `users_email_lower_key` is on `lower(email)`, so the server compares
    // lower-cased and this must too — otherwise "Old@Example.com" submits and
    // comes back 409.
    expect(newEmailProblem('Old@Example.COM', 'old@example.com')).toBe('unchanged');
  });

  it('rejects an address over the column limit', () => {
    expect(newEmailProblem(`${'a'.repeat(320)}@example.com`, 'old@example.com')).toBe('malformed');
  });

  it('accepts a well-formed new address, trimming it', () => {
    expect(newEmailProblem('  new@example.com  ', 'old@example.com')).toBeNull();
  });
});
