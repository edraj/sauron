/**
 * Which value each sortable column of the personal notification-subscriptions
 * table orders by (`components/account/NotificationSubscriptions.svelte`).
 *
 * Lives outside the component for the reason `monitor-sort.ts` gives: vitest
 * runs on the node environment and cannot import a `.svelte` file, so an
 * inline accessor map is untestable — and these accessors are where the
 * interesting mistakes are.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the default column while the caret sits on the header that
 * was clicked. `subscriptionAccessor` warns in dev; the map is a convention,
 * not a guard.
 */
import type { NotificationSubscription, SubscriptionKind } from './index';
import { describeSubscription } from './notification-prefs';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

/** How the "Notify about" cell renders a kind. Supplied by the component. */
export type SubscriptionKindLabel = (kind: SubscriptionKind) => string;

const ACCESSORS: Record<
  string,
  (s: NotificationSubscription, kindLabel: SubscriptionKindLabel) => SortValue
> = {
  // The rendered phrase — `Project “Checkout”` / `App (deleted)` — not the raw
  // `scope_id`, which is a uuid, nor `scope_name`, which is null for a deleted
  // target and would float every orphaned row to one end.
  scope: (s) => describeSubscription(s),
  // The LABEL, not the raw kind. They disagree: raw order is
  // error_new_issue < error_regression < error_spike < uptime, while the cell
  // reads "Error rate increasing", "Issue regressed", "New issue", "Uptime".
  // The map is injected rather than imported because it is the component's
  // display copy and there must be exactly one of it.
  kind: (s, kindLabel) => kindLabel(s.kind),
  // How many environments the subscription is narrowed to. Zero renders as
  // "All", so an ascending sort puts the UNNARROWED rows first — that is the
  // count reading, and the cell shows a count for every other row.
  environments: (s) => s.environment_ids.length,
  // What the user will actually receive, which is what the cell shows; the
  // requested `delivery` may be capped and appears only as a badge.
  //
  // TEXT, and deliberately left that way by Task 5's ranking pass. The comment
  // this replaces said alphabetical order "is not frequency order"; it is.
  // `daily < hourly < immediate` as text is exactly `daily < hourly <
  // immediate` by frequency — the three words happen to be in cadence order —
  // so `rankOf(['daily', 'hourly', 'immediate'])` would produce the identical
  // ordering. A ladder here would add a second place for the order to live
  // without changing a single row, and no test could tell it apart from this
  // accessor: a rank test that cannot fail. A fourth cadence (`weekly` sorts
  // after `immediate` as text but is the least frequent of all) would break the
  // coincidence, and is the point to revisit this.
  delivery: (s) => s.effective_delivery,
  // The window's START MINUTE, not `quietHoursLabel`'s text. "Always on" is an
  // absent window rather than a time of day, and only a null keeps it at one
  // end in BOTH directions — as text it would lead one direction and trail the
  // other depending on whether the collator ranks letters above digits.
  quiet_hours: (s) => s.quiet_start_min,
  // On / Off. The cell has three states — On, Off, and "Off — access removed"
  // — but the third is a REASON for the second, so ordering by the boolean
  // groups the two Off variants together, which is what a reader sorting this
  // column is after. Distinguishing them is a ranking question, i.e. Task 5's.
  state: (s) => s.enabled,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * A REPLACEMENT, not a description: `list_subscriptions_for_user` (backend
 * `repo.rs`) returns `created_at ASC`, and while `created_at` is on the wire it
 * is not a column of this table — a default sort on a column with no header
 * would put the caret nowhere. Scope A-Z is the closest thing to a stable,
 * nameable order the table can show.
 *
 * Exported so the component seeds from the same constant the unknown-key
 * fallback uses.
 */
export const SUBSCRIPTION_DEFAULT_SORT: SortState = { key: 'scope', dir: 'asc' };

export function subscriptionAccessor(
  key: string,
  kindLabel: SubscriptionKindLabel,
): (s: NotificationSubscription) => SortValue {
  let accessor = ACCESSORS[key];
  if (!accessor) {
    if (import.meta.env.DEV) {
      console.warn(
        `[subscription-sort] no accessor for column "${key}" — sorting by ` +
          `"${SUBSCRIPTION_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in ` +
          `subscription-sort.ts.`,
      );
    }
    accessor = ACCESSORS[SUBSCRIPTION_DEFAULT_SORT.key];
  }
  const resolved = accessor;
  return (s) => resolved(s, kindLabel);
}
