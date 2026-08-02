import { describe, expect, it } from 'vitest';
import {
  canCancelPasswordReset,
  canResetMemberPassword,
  isPasswordResetRequired,
  passwordRules,
  readResetToken,
} from './password-reset';
import type { Member, MemberGrant } from './index';

// Fixtures are `Member`, not `MemberGrant`. `MembersTable.svelte` iterates
// `grouped: Member[]`, and structural typing means the wrong annotation
// compiles — the predicates would then be documented and unit-tested against a
// shape the caller never passes.
function grant(overrides: Partial<MemberGrant> = {}): MemberGrant {
  return {
    id: 'g1',
    user_id: 'u1',
    email: 'ada@example.com',
    name: 'Ada',
    role_id: 'r1',
    role_name: 'Viewer',
    scope_type: 'org',
    scope_id: 'o1',
    is_active: true,
    credentials_invalidated_at: null,
    ...overrides,
  };
}

function member(overrides: Partial<Member> = {}): Member {
  return {
    user_id: 'u1',
    email: 'ada@example.com',
    name: 'Ada',
    is_active: true,
    credentials_invalidated_at: null,
    grants: [grant()],
    ...overrides,
  };
}

describe('readResetToken', () => {
  it('reads the token', () => {
    expect(readResetToken('token=abc')).toBe('abc');
  });
  it('reads it from among other params', () => {
    expect(readResetToken('a=1&token=abc&b=2')).toBe('abc');
  });
  it('decodes a percent-encoded value', () => {
    expect(readResetToken('token=a%2Bb')).toBe('a+b');
  });
  it('treats an empty value as absent', () => {
    expect(readResetToken('token=')).toBeNull();
  });
  it('handles an empty and a null query string', () => {
    expect(readResetToken('')).toBeNull();
    expect(readResetToken(null)).toBeNull();
  });
});

describe('passwordRules', () => {
  it('flags a short password only once something is typed', () => {
    expect(passwordRules('', '').tooShort).toBe(false);
    expect(passwordRules('abc', '').tooShort).toBe(true);
  });
  it('flags a mismatch only once the confirm field is touched', () => {
    expect(passwordRules('correcthorse', '').mismatch).toBe(false);
    expect(passwordRules('correcthorse', 'correcthors').mismatch).toBe(true);
  });
  it('allows submit only when both fields agree and are long enough', () => {
    expect(passwordRules('correcthorse', 'correcthorse').canSubmit).toBe(true);
    expect(passwordRules('short', 'short').canSubmit).toBe(false);
    expect(passwordRules('correcthorse', 'other').canSubmit).toBe(false);
  });
});

describe('isPasswordResetRequired', () => {
  it('matches the real error shape', () => {
    expect(
      isPasswordResetRequired({
        status: 403,
        code: 'password_reset_required',
        message: 'x',
        isNetwork: false,
      }),
    ).toBe(true);
  });
  it('does not match the temp-password gate, which the two names invite', () => {
    expect(
      isPasswordResetRequired({
        status: 403,
        code: 'password_change_required',
        message: 'x',
        isNetwork: false,
      }),
    ).toBe(false);
  });
  it('does not match a non-error', () => {
    expect(isPasswordResetRequired(new Error('boom'))).toBe(false);
    expect(isPasswordResetRequired(null)).toBe(false);
  });
});

describe('canResetMemberPassword / canCancelPasswordReset', () => {
  it('offers reset for an ordinary active member', () => {
    expect(canResetMemberPassword(member(), 'me', true)).toBe(true);
    expect(canCancelPasswordReset(member(), 'me', true)).toBe(false);
  });
  it('offers neither without the permission', () => {
    expect(canResetMemberPassword(member(), 'me', false)).toBe(false);
    expect(canCancelPasswordReset(member({ credentials_invalidated_at: 'x' }), 'me', false)).toBe(
      false,
    );
  });
  it('offers neither for yourself — the server answers 409', () => {
    expect(canResetMemberPassword(member({ user_id: 'me' }), 'me', true)).toBe(false);
    expect(
      canCancelPasswordReset(member({ user_id: 'me', credentials_invalidated_at: 'x' }), 'me', true),
    ).toBe(false);
  });
  it('offers neither for a deactivated member — the server answers 409', () => {
    expect(canResetMemberPassword(member({ is_active: false }), 'me', true)).toBe(false);
    expect(
      canCancelPasswordReset(member({ is_active: false, credentials_invalidated_at: 'x' }), 'me', true),
    ).toBe(false);
  });
  it('swaps reset for cancel once one is pending', () => {
    const pending = member({ credentials_invalidated_at: '2026-08-01T00:00:00Z' });
    expect(canResetMemberPassword(pending, 'me', true)).toBe(false);
    expect(canCancelPasswordReset(pending, 'me', true)).toBe(true);
  });
  it('never offers both — the row carries one menu item, not two that contradict', () => {
    for (const m of [
      member(),
      member({ credentials_invalidated_at: 'x' }),
      member({ is_active: false }),
      member({ user_id: 'me' }),
    ]) {
      expect(canResetMemberPassword(m, 'me', true) && canCancelPasswordReset(m, 'me', true)).toBe(
        false,
      );
    }
  });
});
