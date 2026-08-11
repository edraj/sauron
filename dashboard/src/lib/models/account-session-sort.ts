/**
 * Which value each sortable column of the Account page's sessions table orders
 * by.
 *
 * A separate module from `account-sessions.ts` — which answers the copy
 * question ("how do I phrase this device?") and the summary questions
 * (`otherSessionCount`, `allSameIp`) — so that every table in slice 4 keeps the
 * same shape: one `<page>-sort.ts` exporting an accessor lookup and a default.
 * It imports `describeSession` from there rather than restating it, because the
 * Device column must sort by exactly the text it renders.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the default column while the caret sits on the header that
 * was clicked. `sessionAccessor` warns in dev; the map is a convention, not a
 * guard.
 */
import { describeSession } from './account-sessions';
import type { AccountSession } from './index';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

const ACCESSORS: Record<string, (s: AccountSession) => SortValue> = {
  // The rendered phrase, not `user_agent`. The cell shows "Firefox on Fedora",
  // which is built from `browser`/`os` and only falls back to the raw UA; an
  // accessor reading `user_agent` would order the column by a string most rows
  // do not display.
  device: (s) => describeSession(s),
  // Nullable and NOT coerced to '': an unknown address is absent, not the
  // lowest one, so `sortRows` keeps it last in both directions. Dotted quads
  // compare well here because the shared collator runs with `numeric: true`,
  // so 10.0.0.9 precedes 10.0.0.10 rather than following it.
  ip: (s) => s.ip,
  signed_in: (s) => s.created_at,
  /**
   * The instant the row's "Last used" cell actually shows.
   *
   * A live row shows `last_used_at`; a revoked row shows "Signed out
   * <revoked_at>" in the same column. Ordering every row by `last_used_at`
   * would order the revoked ones by a timestamp they do not display — the
   * "sorts by one number, shows another" shape the spec rules out for Issues.
   * `revoked_at` is null on live rows, so this is `last_used_at` for them.
   */
  last_used: (s) => s.revoked_at ?? s.last_used_at,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * This DESCRIBES the endpoint: `list_auth_sessions` (backend `repo.rs`) orders
 * by `last_used_at DESC`. It replaces the page's own `sortSessions(...)` call,
 * which applied the same ordering with the caller's own session forced to the
 * top — that one extra rule is what changes, and it goes because two orderings
 * applied in sequence is a bug waiting for someone to change one of them. The
 * current device is still marked, by its "This device" badge.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses.
 */
export const SESSION_DEFAULT_SORT: SortState = { key: 'last_used', dir: 'desc' };

export function sessionAccessor(key: string): (s: AccountSession) => SortValue {
  const accessor = ACCESSORS[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[account-session-sort] no accessor for column "${key}" — sorting by ` +
        `"${SESSION_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in ` +
        `account-session-sort.ts.`,
    );
  }
  return ACCESSORS[SESSION_DEFAULT_SORT.key];
}
