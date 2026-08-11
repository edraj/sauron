/**
 * Which value each sortable column of the three Alerts tables orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are.
 *
 * THREE independent sets, never one shared map: `Alerts.svelte` renders three
 * tables (channels, rules, history), and a shared sort would make ordering the
 * channels list reorder the rules list under a header nobody clicked.
 *
 * Be exact about how much a map buys, because it is less than it looks:
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header. It
 * falls through to the table's default column, so the table quietly re-sorts by
 * something else while the caret sits on the header that was clicked — a
 * confident wrong answer, not an obvious breakage. `pick` warns in dev to make
 * that case say something; the map itself is a convention, not a guard.
 */
import type {
  AlertEvent,
  AlertRule,
  AlertSeverity,
  ChannelKind,
  NotificationChannel,
  TriggerType,
} from './index';
import type { SortState } from './sort';
import { rankOf, type SortValue } from './sort-rows';

/**
 * Look `key` up, or fall back to `fallback`'s column and say so in dev.
 *
 * PRIVATE to this module and duplicated in the other `*-sort.ts` files rather
 * than shared: Task 2 deliberately rejected a cross-module `accessorFor`
 * helper, on the grounds that 19 tables copying a fallback line is clearer than
 * an indirection every one of them has to learn. What is NOT worth copying is
 * the same six-line dev warning three times inside one file, which is all this
 * collapses. The warning is stripped from production builds — `import.meta.env.DEV`
 * is replaced with a literal at build time.
 *
 * Generic over the accessor's whole parameter list `A`, not over a single row
 * type: two of this file's three accessor maps take a second argument (the
 * page's display-copy label — `KindLabel`, `TriggerLabel`), and a `(row: T) =>
 * SortValue` signature cannot describe those. Typed that way, every one of the
 * three call sites failed with "Expected 1 arguments, but got 2" and the
 * accessor map was rejected as unassignable. `(...args: A) => SortValue` admits
 * the one-argument maps unchanged — `A` simply infers as a one-element tuple —
 * so nothing here needs a cast or an `any` to pass both shapes through.
 */
function pick<A extends unknown[]>(
  table: string,
  map: Record<string, (...args: A) => SortValue>,
  key: string,
  fallback: SortState,
): (...args: A) => SortValue {
  const accessor = map[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[alert-sort] no accessor for ${table} column "${key}" — sorting by ` +
        `"${fallback.key}" instead. Add it to alert-sort.ts.`,
    );
  }
  return map[fallback.key];
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/** How the Type cell renders a `ChannelKind`. Supplied by the page. */
export type KindLabel = (kind: ChannelKind) => string;

const CHANNEL_ACCESSORS: Record<
  string,
  (c: NotificationChannel, kindLabel: KindLabel) => SortValue
> = {
  name: (c) => c.name,
  // The LABEL, not the raw `kind`. The two disagree: raw order puts
  // `matrix` between `email` and `slack`, while the cell reads
  // "Element / Matrix" and belongs under E. Sorting by the raw enum would
  // leave the visible column looking unsorted for one row in six, which is
  // the confident-wrong-answer shape this file exists to avoid. The map is
  // injected rather than imported because it is the page's display copy and
  // there must be exactly one of it.
  type: (c, kindLabel) => kindLabel(c.kind),
  // The boolean, not the word the badge renders. `sortRows` puts `false`
  // first ascending, which is also "disabled" before "enabled" — the two
  // agree here, and the boolean cannot drift if the wording changes.
  status: (c) => c.enabled,
};

/**
 * The order the channels table is in before anyone clicks a header.
 *
 * This does NOT describe the endpoint's own ordering: `list_channels_for_org`
 * (backend `repo.rs`) returns `created_at DESC`, so seeding Name A-Z CHANGES
 * the initial order. Deliberate — `sort.ts` has no "unsorted" state to fall
 * back to, and a page whose default column is one nobody can see (there is no
 * Created column here) would put the caret nowhere.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const CHANNEL_DEFAULT_SORT: SortState = { key: 'name', dir: 'asc' };

export function channelAccessor(
  key: string,
  kindLabel: KindLabel,
): (c: NotificationChannel) => SortValue {
  const accessor = pick('channels', CHANNEL_ACCESSORS, key, CHANNEL_DEFAULT_SORT);
  return (c) => accessor(c, kindLabel);
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/** How the Trigger cell renders a `TriggerType`. Supplied by the page. */
export type TriggerLabel = (trigger: TriggerType) => string;

/**
 * Alert severity, LEAST severe first — the direction `rankOf` documents, so a
 * higher rank is worse and `desc` puts `critical` at the top.
 *
 * As text the order is `critical, info, warning`, which reads as a working sort
 * and puts `info` above `warning` while claiming to be sorted by severity.
 *
 * Annotated `readonly AlertSeverity[]` so a misspelt or non-member severity is
 * a compile error. That cannot catch an OMITTED one — a two-element array is
 * still a valid `readonly AlertSeverity[]` — so `alert-sort.test.ts` pins the
 * ladder against a `Record<AlertSeverity, number>`, which is the thing that
 * stops compiling when a fourth severity joins the union.
 */
const SEVERITY_ORDER: readonly AlertSeverity[] = ['info', 'warning', 'critical'];
const severityRank = rankOf(SEVERITY_ORDER);

const RULE_ACCESSORS: Record<string, (r: AlertRule, triggerLabel: TriggerLabel) => SortValue> = {
  name: (r) => r.name,
  // The label, for the same reason the channels Type column uses one: raw
  // `trigger_type` order (`error_spike, error_threshold, event_threshold,
  // issue_new, …`) has almost nothing in common with label order
  // ("Error count crosses threshold", "Error rate spikes", "Event count …",
  // "Latency degrades", …).
  trigger: (r, triggerLabel) => triggerLabel(r.trigger_type),
  // The RANK, not the word — see `SEVERITY_ORDER` above. A severity this build
  // does not know ranks null and sorts last in both directions rather than
  // claiming to be the most or least severe thing on the page.
  severity: (r) => severityRank(r.severity),
  // How many channels this rule fans out to — the count the cell renders.
  // The brief described this column as a chip list and ruled it out; the chips
  // are in the rule FORM, and the table cell is `r.channel_ids.length`, which
  // is structurally the same column as the mask audit trail's Targets. Sorting
  // one and not the other would be an inconsistency inside one slice.
  channels: (r) => r.channel_ids.length,
  // Seconds, not the "300s" the cell renders — text would order 1200s before
  // 300s.
  throttle: (r) => r.throttle_seconds,
  status: (r) => r.enabled,
};

/**
 * Also a REPLACEMENT, not a description: `list_alert_rules_for_org` returns
 * `created_at DESC`. See `CHANNEL_DEFAULT_SORT` for why a visible column has
 * to be the seed.
 */
export const RULE_DEFAULT_SORT: SortState = { key: 'name', dir: 'asc' };

export function ruleAccessor(
  key: string,
  triggerLabel: TriggerLabel,
): (r: AlertRule) => SortValue {
  const accessor = pick('rules', RULE_ACCESSORS, key, RULE_DEFAULT_SORT);
  return (r) => accessor(r, triggerLabel);
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/**
 * Resolves a channel id to its name, or `null` when there is no name to show —
 * a delivery whose `channel_id` is null, or one whose channel has since been
 * deleted.
 *
 * `null` and not the em dash the cell renders. An absent channel is an absent
 * value, not a small one: returned as `'—'` it would collate before every real
 * name, leading the ascending sort and trailing the descending one, which is
 * exactly the asymmetry `emailOrNull` was written to prevent in
 * `pii-inspector-sort.ts`. `sortRows` puts a null last in BOTH directions.
 * The page keeps the dash where it belongs — in the cell.
 */
export type ChannelName = (id: string | null) => string | null;

/**
 * Delivery outcome, LEAST worth looking at first — same direction as
 * `SEVERITY_ORDER`, so `desc` leads with the deliveries that did not arrive.
 *
 * The axis is "did the person get told, and is that a problem": `sent` arrived;
 * `skipped` was deliberately not attempted (a disabled channel, quiet hours) so
 * nothing is wrong; `throttled` was suppressed by the rule's own rate limit, so
 * a real alert may have gone unseen; `failed` was attempted and broke, which is
 * the row someone opens this table for.
 *
 * The middle pair is the judgement call and is stated rather than hidden:
 * `throttled` above `skipped` because a throttle is a consequence of volume and
 * a skip is a setting. Alphabetically the four run `failed, sent, skipped,
 * throttled` — `failed` leads the ASCENDING sort by spelling alone, which is
 * the kind of accident that gets mistaken for a working ranking.
 */
const DELIVERY_ORDER: readonly AlertEvent['status'][] = [
  'sent',
  'skipped',
  'throttled',
  'failed',
];
const deliveryRank = rankOf(DELIVERY_ORDER);

const EVENT_ACCESSORS: Record<string, (h: AlertEvent, channelName: ChannelName) => SortValue> = {
  // The raw ISO instant. `sortRows` compares ISO-8601 as bytes, which is
  // chronological; the cell's `toLocaleString` is not — under most locales it
  // is day-first or month-first text.
  when: (h) => h.created_at,
  title: (h) => h.title,
  // The resolved NAME, or null — see `ChannelName`. `channel_id` is a uuid, so
  // ordering by it would produce a column in visibly random order that still
  // looks like a sort. The resolver is injected because it reads the page's
  // loaded channel list, which this module has no access to.
  channel: (h, channelName) => channelName(h.channel_id),
  // The RANK, not the word — see `DELIVERY_ORDER` above.
  status: (h) => deliveryRank(h.status),
  attempts: (h) => h.attempts,
};

/**
 * Unlike the other two, this seed MATCHES the endpoint: `list_alert_events_visible`
 * returns `created_at DESC`, and `created_at` is the When column, so the
 * history table opens in exactly the order it does today.
 */
export const EVENT_DEFAULT_SORT: SortState = { key: 'when', dir: 'desc' };

export function alertEventAccessor(
  key: string,
  channelName: ChannelName,
): (h: AlertEvent) => SortValue {
  const accessor = pick('history', EVENT_ACCESSORS, key, EVENT_DEFAULT_SORT);
  return (h) => accessor(h, channelName);
}
