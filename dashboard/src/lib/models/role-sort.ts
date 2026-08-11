/**
 * Which value each sortable column of the Roles table orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the default column while the caret sits on the header that
 * was clicked. `roleAccessor` warns in dev to make that audible; the map itself
 * is a convention, not a guard.
 */
import type { Role } from './index';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

/**
 * Distinct members per role id, keyed by role — the page's own
 * `roleMemberCounts`, injected rather than recomputed here.
 *
 * It is derived from the members list, which this module has no business
 * loading, and there must be exactly one definition of "how many people hold
 * this role" or the column can order by a different number than it displays.
 */
export type RoleMemberCounts = Record<string, number>;

const ACCESSORS: Record<string, (r: Role, counts: RoleMemberCounts) => SortValue> = {
  // The name alone. The cell also carries a "system" badge, but that is a
  // property of the role rather than part of its name — ordering by
  // `is_system` first would reproduce the server's grouping under a header
  // labelled Name, which is not what the header says it does.
  name: (r) => r.name,
  // Nullable and NOT coerced to '': a role with no description renders an em
  // dash, and an empty string would sort it to the top of an ascending list as
  // though it were named "". `sortRows` keeps an absent value last both ways.
  description: (r) => r.description,
  // The COUNT, which is exactly what the cell renders. The permissions
  // themselves are a list, and a column ordered by "whichever permission comes
  // first alphabetically" would be worse than one that does not sort at all.
  permissions: (r) => r.permissions.length,
  /**
   * The member count the cell shows, including its `?? 0`.
   *
   * This is the one justified `?? 0` on the page: a role absent from the map
   * is a role nobody holds, which is genuinely zero members and is displayed
   * as `0`. Passing the `undefined` through would push every unused role to
   * the bottom in BOTH directions while its cell reads 0 — a column ordered by
   * something other than what it displays.
   */
  members: (r, counts) => counts[r.id] ?? 0,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * This REPLACES the endpoint's ordering rather than describing it:
 * `list_roles` (backend `repo.rs`) orders by `is_system DESC, name ASC`, so
 * the presets used to lead the table and custom roles followed. `is_system` is
 * not a column here — it is a badge inside the Name cell — and seeding a sort
 * on something with no header would put the caret nowhere, so the table now
 * opens as one flat A-Z list. The badge still marks which rows are presets.
 * The same trade `account-session-sort.ts` made when it dropped
 * "current session first".
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const ROLE_DEFAULT_SORT: SortState = { key: 'name', dir: 'asc' };

export function roleAccessor(
  key: string,
  counts: RoleMemberCounts,
): (r: Role) => SortValue {
  const accessor = ACCESSORS[key];
  if (accessor) return (r) => accessor(r, counts);
  if (import.meta.env.DEV) {
    console.warn(
      `[role-sort] no accessor for column "${key}" — sorting by ` +
        `"${ROLE_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in role-sort.ts.`,
    );
  }
  const fallback = ACCESSORS[ROLE_DEFAULT_SORT.key];
  return (r) => fallback(r, counts);
}
