import { describe, expect, it } from 'vitest';
import { canRevokeMemberKeys, RESET_REVOCATION_NOTICE_DAYS, recentResetRevocation, revokedReasonKey } from './api-keys';
import { groupMembers, NO_MEMBER_API_KEYS, type Member, type MemberGrant } from './index';

const NOW = new Date('2026-11-10T12:00:00Z');
const daysAgo = (n: number) => new Date(NOW.getTime() - n * 86_400_000).toISOString();

describe('recentResetRevocation', () => {
  it('flags a reset revocation inside the notice window, with its time', () => {
    const at = daysAgo(3);
    expect(recentResetRevocation({ active: 0, revoked_by_password_reset_at: at }, NOW)).toBe(at);
    const edge = daysAgo(RESET_REVOCATION_NOTICE_DAYS);
    expect(recentResetRevocation({ active: 0, revoked_by_password_reset_at: edge }, NOW)).toBe(edge);
  });

  it('drops one older than the window, and never flags a member with none', () => {
    const old = daysAgo(RESET_REVOCATION_NOTICE_DAYS + 1);
    expect(recentResetRevocation({ active: 2, revoked_by_password_reset_at: old }, NOW)).toBeNull();
    expect(recentResetRevocation(NO_MEMBER_API_KEYS, NOW)).toBeNull();
  });

  it('shows an unparseable timestamp rather than hiding a real event', () => {
    expect(recentResetRevocation({ active: 0, revoked_by_password_reset_at: 'garbage' }, NOW)).toBe('garbage');
  });
});

describe('revokedReasonKey', () => {
  it('names each recorded reason, and anything else as plain revoked', () => {
    expect(revokedReasonKey('user_revoked')).toBe('account.apiKeys.reason.userRevoked');
    expect(revokedReasonKey('admin_revoked')).toBe('account.apiKeys.reason.adminRevoked');
    expect(revokedReasonKey('reset_forced')).toBe('account.apiKeys.reason.resetForced');
    expect(revokedReasonKey('password_reset')).toBe('account.apiKeys.reason.passwordReset');
    expect(revokedReasonKey(null)).toBe('account.apiKeys.reason.unknown');
    expect(revokedReasonKey('from_the_future')).toBe('account.apiKeys.reason.unknown');
  });
});

describe('canRevokeMemberKeys', () => {
  const member = (user_id: string): Member => ({
    user_id,
    email: `${user_id}@example.com`,
    name: null,
    is_active: true,
    credentials_invalidated_at: null,
    pending_email_change: null,
    api_keys: NO_MEMBER_API_KEYS,
    grants: [],
  });

  it('offers everyone but the signed-in admin', () => {
    expect(canRevokeMemberKeys(member('other'), 'me')).toBe(true);
    expect(canRevokeMemberKeys(member('me'), 'me')).toBe(false);
  });
});

describe('groupMembers api_keys', () => {
  const grant = (over: Partial<MemberGrant>): MemberGrant => ({
    id: 'g1',
    user_id: 'u1',
    email: 'u1@example.com',
    name: null,
    role_id: 'r1',
    role_name: 'Viewer',
    scope_type: 'org',
    scope_id: 'o1',
    is_active: true,
    credentials_invalidated_at: null,
    ...over,
  });

  it('carries the summary, and normalizes an older server that omits it', () => {
    const keys = { active: 2, revoked_by_password_reset_at: null };
    const [withKeys, without] = groupMembers([
      grant({ user_id: 'a', api_keys: keys }),
      grant({ id: 'g2', user_id: 'b' }),
    ]);
    expect(withKeys.api_keys).toEqual(keys);
    expect(without.api_keys).toEqual(NO_MEMBER_API_KEYS);
  });
});
