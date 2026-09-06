/**
 * Option lists for the alert rule dialog's narrowing filters.
 *
 * Extracted from `pages/Alerts.svelte` so the one rule that matters here is
 * testable: a rule's SAVED filter value must survive being edited, even when
 * the offered list does not contain it.
 */

/**
 * `options` with `selected` spliced in front when it is missing from them.
 *
 * A native `<select>` whose bound value matches no `<option>` writes its first
 * option back into the binding. Without this pin, opening — not even saving —
 * a rule narrowed to a value the current list cannot offer (an environment
 * since retired, an environment from another project, an `op` no SDK in this
 * vocabulary emits) silently rewrites the rule to "any", and the next save
 * persists that.
 *
 * The same defect the audit-log facets hit (`withSelected` in `./audit.ts`),
 * kept separate from it because these options are bare strings rather than
 * `{ id, label }` pairs.
 *
 * An empty `selected` is the "any" case, which each dialog renders as its own
 * fixed first option — never pinned, or every rule would grow a blank entry.
 */
export function withPinnedChoice(options: string[], selected: string): string[] {
  if (!selected || options.includes(selected)) return options;
  return [selected, ...options];
}

/**
 * Merge the two sources the environment picker draws on, deduped and ordered.
 *
 * The project CATALOGUE names every environment any app in the project
 * defines; the SESSION list is the current app's enrollments. Neither alone
 * covers every reader — see `loadEnvironmentOptions` in `pages/Alerts.svelte`
 * for why — and the union is deduped by name because an app is normally
 * enrolled in exactly the catalogue entries the first list already returned.
 */
export function mergeEnvironmentNames(catalogue: string[], session: string[]): string[] {
  return [...new Set([...catalogue, ...session])].sort((a, b) => a.localeCompare(b));
}

/**
 * The transaction `op` values a latency rule can narrow to.
 *
 * The same five `pages/Performance.svelte`'s own filter offers, and the same
 * five `envelope.rs`'s `TransactionItem::op` documents — kept as a literal
 * rather than derived from live data because a rule may span several apps and
 * a dialog is the wrong place to pay for a `performance/summary` scan.
 *
 * Deliberately NOT presented as exhaustive: `op` is a free-form `String` on the
 * wire with no server-side coercion (only the JS SDK normalizes to `custom`),
 * so a rule can legitimately carry an op outside this list. That is exactly
 * what `withPinnedChoice` is for on this field.
 */
export const TRANSACTION_OPS = [
  'navigation',
  'http',
  'resource',
  'screen_load',
  'custom',
] as const;

/** `screen_load` → `screen load`, matching `Performance.svelte`'s `opLabel`. */
export function opLabel(op: string): string {
  return op.replace('_', ' ');
}

/** Which extra condition inputs a trigger actually uses. */
export interface TriggerNeeds {
  threshold: boolean;
  window: boolean;
  monitor: boolean;
  spike: boolean;
  metric: boolean;
  op: boolean;
  env: boolean;
  level: boolean;
  eventName: boolean;
  query: boolean;
}

/**
 * The single source of truth for which fields the rule dialog offers.
 *
 * Extracted from `pages/Alerts.svelte` because one of these flags — `env` —
 * had been missing entirely, and the field was rendered unconditionally. That
 * is how `perf_degradation` shipped a filter it never applied, and how the two
 * monitor triggers still offer one that nothing on the server can ever read.
 *
 * **A field belongs here only if something consumes it.** The rule for `env`
 * is the evaluator's own `TriggerType::is_metric()`: the two monitor triggers
 * are dispatched inline by `sauron-monitor`, which loads a rule's channels and
 * severity and never looks at `conditions` at all — and `monitors` is
 * project-scoped with no `environment_id` to filter by in the first place.
 */
export function triggerNeeds(t: string): TriggerNeeds {
  const monitor = t === 'monitor_down' || t === 'monitor_up';
  return {
    threshold: t === 'error_threshold' || t === 'event_threshold' || t === 'perf_degradation',
    window: !monitor,
    monitor,
    spike: t === 'error_spike',
    metric: t === 'perf_degradation',
    // Only the latency trigger reads `filters.op` — it is the only one that
    // queries `transactions`, the only table carrying the column.
    op: t === 'perf_degradation',
    // Everything the evaluator polls resolves `filters.environment` to
    // enrollment ids and narrows with them; the prober-driven pair cannot.
    env: !monitor,
    level:
      t === 'issue_new' ||
      t === 'issue_regression' ||
      t === 'error_threshold' ||
      t === 'error_spike',
    eventName: t === 'event_threshold',
    query: t === 'error_threshold' || t === 'error_spike',
  };
}

/**
 * Which `conditions.filters` key each dialog field owns.
 *
 * The backend reads seven (`rule.rs`'s `Filters::from_value`): the five here
 * plus `tag_key`/`tag_value`, which have no field at all. A key absent from
 * this map is one the dialog does not manage, and
 * [`mergeStoredFilters`] never touches it.
 */
const FIELD_FOR_FILTER: Record<string, keyof TriggerNeeds> = {
  level: 'level',
  environment: 'env',
  event_name: 'eventName',
  op: 'op',
  query: 'query',
};

/**
 * Fold the filters the dialog's own fields produced back together with the
 * ones already stored on the rule.
 *
 * Two kinds of stored key survive a save the dialog cannot express:
 *
 *  - **Keys no field owns.** `tag_key`/`tag_value` are read by the evaluator
 *    and narrow a real count, and nothing in this dialog can show or set them.
 *    Dropping them on an unrelated edit silently changes what the rule counts.
 *  - **Keys whose field this trigger hides.** `filters.environment` on a
 *    `monitor_down` rule is inert — `monitors` has no environment and the
 *    prober never reads `conditions` — but inert is not the same as unwanted,
 *    and a save that touched the name has no business deleting it.
 *
 * The dialog wins only where it is actually showing the field: a `needs` flag
 * that is true means an empty input is a deliberate clear, so the key is
 * dropped. That is what keeps "clear the environment filter" working on the
 * triggers that offer one.
 */
export function mergeStoredFilters(
  built: Record<string, string>,
  stored: Record<string, string>,
  needs: TriggerNeeds,
): Record<string, string> {
  const merged = { ...built };
  for (const [key, value] of Object.entries(stored)) {
    if (key in merged || !value) continue;
    const field = FIELD_FOR_FILTER[key];
    // Shown-and-empty is a clear. Hidden, or unowned, is preserved.
    if (field && needs[field]) continue;
    merged[key] = value;
  }
  return merged;
}
