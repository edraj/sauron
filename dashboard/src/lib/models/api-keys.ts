import type { MessageKey } from '../i18n';
import type { Member, MemberApiKeys } from './index';

/**
 * How long the Members page flags that a forgotten-password reset (the emailed
 * link) revoked a member's personal API keys. Long enough for an admin to
 * notice a member's MCP client stopped working and see why; short enough that
 * the badge does not become permanent furniture.
 */
export const RESET_REVOCATION_NOTICE_DAYS = 30;

const DAY_MS = 86_400_000;

/**
 * When a forgotten-password reset revoked this member's keys, if that was
 * within {@link RESET_REVOCATION_NOTICE_DAYS} of `now`; else null. An
 * unparseable or future-skewed timestamp still shows (the server's clock is
 * the one that matters, and hiding a real event is the worse failure).
 */
export function recentResetRevocation(keys: MemberApiKeys, now: Date = new Date()): string | null {
  const at = keys.revoked_by_password_reset_at;
  if (!at) return null;
  const when = Date.parse(at);
  if (Number.isNaN(when)) return at;
  return now.getTime() - when <= RESET_REVOCATION_NOTICE_DAYS * DAY_MS ? at : null;
}

/**
 * The label for why a key was revoked (`api_keys.revoked_reason`). Keys
 * revoked before the server recorded reasons, and any reason a newer server
 * adds, read as plain "Revoked" rather than a guess.
 */
export function revokedReasonKey(reason: string | null | undefined): MessageKey {
  switch (reason) {
    case 'user_revoked':
      return 'account.apiKeys.reason.userRevoked';
    case 'admin_revoked':
      return 'account.apiKeys.reason.adminRevoked';
    case 'reset_forced':
      return 'account.apiKeys.reason.resetForced';
    case 'password_reset':
      return 'account.apiKeys.reason.passwordReset';
    default:
      return 'account.apiKeys.reason.unknown';
  }
}

/**
 * Whether the Members table offers a checkbox (and the row action) for
 * revoking this member's keys. Everyone but the signed-in admin: the server
 * refuses a self-target outright (own keys are revoked from the Account
 * page). Who outranks whom, and who also belongs to another organization,
 * only the server can say; it refuses those by name.
 */
export function canRevokeMemberKeys(member: Member, currentUserId: string): boolean {
  return member.user_id !== currentUserId;
}
