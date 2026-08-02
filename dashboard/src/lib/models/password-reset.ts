import { isNormalizedError } from '../api/client';
import type { Member } from './index';

/**
 * Read the reset token out of a hash-fragment query string.
 *
 * The token lives in the fragment precisely so it never reaches a server log,
 * a proxy log or an analytics beacon, so this is the only place it is parsed.
 */
export function readResetToken(qs: string | null): string | null {
  const raw = new URLSearchParams(qs ?? '').get('token');
  const trimmed = raw?.trim() ?? '';
  return trimmed.length > 0 ? trimmed : null;
}

export interface PasswordRules {
  tooShort: boolean;
  mismatch: boolean;
  canSubmit: boolean;
}

/**
 * ChangePassword.svelte's derivations minus `reused` — there is no current
 * password on the reset page. One definition, shared by both screens, so the
 * two cannot drift into disagreeing about what a valid password is.
 */
export function passwordRules(next: string, confirm: string): PasswordRules {
  return {
    tooShort: next.length > 0 && next.length < 8,
    mismatch: confirm.length > 0 && confirm !== next,
    canSubmit: next.length >= 8 && confirm === next,
  };
}

/**
 * True for the API's 403 `password_reset_required` — the twin of
 * `isPasswordChangeRequired` in the auth store.
 *
 * Lives here rather than in that store because the login page is its only
 * caller and the store has no reason to know.
 */
export function isPasswordResetRequired(err: unknown): boolean {
  return isNormalizedError(err) && err.status === 403 && err.code === 'password_reset_required';
}

/** An older server build omits the field entirely, so this is a truthiness
    check rather than `!== null`. */
function resetPending(member: Member): boolean {
  return Boolean(member.credentials_invalidated_at);
}

/**
 * Mirrors the server's refusals, so the action is never offered for something
 * the server will reject with a 409: self, inactive, or already pending.
 */
export function canResetMemberPassword(
  member: Member,
  currentUserId: string,
  canCredential: boolean,
): boolean {
  if (!canCredential) return false;
  if (member.user_id === currentUserId) return false;
  if (!member.is_active) return false;
  return !resetPending(member);
}

/**
 * The same three guards, but true only when a reset **is** pending. At most one
 * of the two predicates holds for a given member, which is what lets the row
 * carry one menu item instead of two that contradict each other.
 */
export function canCancelPasswordReset(
  member: Member,
  currentUserId: string,
  canCredential: boolean,
): boolean {
  if (!canCredential) return false;
  if (member.user_id === currentUserId) return false;
  if (!member.is_active) return false;
  return resetPending(member);
}
