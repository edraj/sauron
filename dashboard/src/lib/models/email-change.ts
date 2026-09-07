import type { Member } from './index';

/**
 * Read the change token out of a hash-fragment query string.
 *
 * The token lives in the fragment precisely so it is never sent in a request
 * line or a `Referer` — it reaches no server log, proxy log or analytics
 * beacon — so this is the only place it is parsed. Read ONCE at page init,
 * never reactively, so a later navigation cannot swap it mid-submit.
 *
 * Deliberately a second function rather than a reuse of `readResetToken`: these
 * are different flows with different pages, and one shared parser would be a
 * standing invitation to give them one shared token shape.
 */
export function readEmailChangeToken(qs: string | null): string | null {
  const raw = new URLSearchParams(qs ?? '').get('token');
  const trimmed = raw?.trim() ?? '';
  return trimmed.length > 0 ? trimmed : null;
}

/** An older server build omits the field entirely, so this is a truthiness
    check rather than `!== null` — same reasoning as `resetPending`. */
export function emailChangePending(member: Member): boolean {
  return Boolean(member.pending_email_change);
}

/**
 * Mirrors the server's refusals, so the action is never offered for something
 * the server will reject: self (409), inactive (409), or already pending.
 *
 * "Already pending" is not a server refusal on the request route — a second
 * request supersedes the first — but offering both actions on one row would
 * make the pending badge and the menu contradict each other. Withdraw is the
 * action that belongs on a pending row; correcting a mistyped address means
 * withdrawing and asking again, which is the sequence the badge describes.
 */
export function canRequestEmailChange(
  member: Member,
  currentUserId: string,
  canCredential: boolean,
): boolean {
  if (!canCredential) return false;
  if (member.user_id === currentUserId) return false;
  if (!member.is_active) return false;
  return !emailChangePending(member);
}

/**
 * The same guards, but true only when a change **is** pending. At most one of
 * the two predicates holds for a given member, which is what lets the row carry
 * one menu item instead of two that contradict each other.
 */
export function canWithdrawEmailChange(
  member: Member,
  currentUserId: string,
  canCredential: boolean,
): boolean {
  if (!canCredential) return false;
  if (member.user_id === currentUserId) return false;
  if (!member.is_active) return false;
  return emailChangePending(member);
}

/** Why an address cannot be submitted, or `null` when it can. */
export type NewEmailProblem = 'empty' | 'malformed' | 'unchanged';

/**
 * The server's own three refusals, applied before the request goes out.
 *
 * The lower-casing is not cosmetic: `users_email_lower_key` is on
 * `lower(email)` and the handler compares lower-cased, so a dialog that
 * compared raw strings would happily submit "Old@Example.com" for a member
 * whose address is "old@example.com" and get a 409 back.
 *
 * 320 is the column's practical ceiling and the handler's own check.
 */
export function newEmailProblem(input: string, currentEmail: string): NewEmailProblem | null {
  const next = input.trim().toLowerCase();
  if (next.length === 0) return 'empty';
  if (!next.includes('@') || next.length > 320) return 'malformed';
  if (next === currentEmail.trim().toLowerCase()) return 'unchanged';
  return null;
}
