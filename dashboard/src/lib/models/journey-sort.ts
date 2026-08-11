/**
 * Which value each sortable column of the Journeys "Top transitions" table
 * orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the default column while the caret sits on the header that
 * was clicked. `journeyTransitionAccessor` warns in dev to make that audible;
 * the map itself is a convention, not a guard.
 */
import type { JourneyLink } from './index';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

const ACCESSORS: Record<string, (l: JourneyLink) => SortValue> = {
  /**
   * The event AND its step, because the From cell renders both — the event
   * name with a faint "step N" tag beside it.
   *
   * Ordering by `from_event` alone would leave the step tags in arbitrary
   * order INSIDE each event name, which reads as a partly-broken sort; the
   * same reasoning `artifact-sort.ts` gives for its platform/arch pair. The
   * step is padded through the shared collator's `numeric: true` mode rather
   * than by hand, so "step 2" precedes "step 10" — see `sort-rows.ts`.
   *
   * `+ 1` matches the cell: `from_step` is 0-based on the wire and displayed
   * 1-based. It cannot change the ordering (adding one to every value is
   * monotonic), and it is here so the accessor and the cell cannot drift into
   * describing different things.
   */
  from: (l) => `${l.from_event} step ${l.from_step + 1}`,
  // Just the event: the To cell has no step tag, because a transition's
  // destination step is always `from_step + 1`.
  to: (l) => l.to_event,
  // The count, never `toLocaleString()`'s "1,234" — as text that sorts before
  // "999".
  users: (l) => l.count,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * This DESCRIBES what the page already does: `topTransitions` is the ten
 * highest-count links, so `{ users, desc }` reproduces the table exactly as it
 * opens today, with the caret naming the column that produced it.
 *
 * The page keeps its own count-ordered `.sort()` above the slice, and that is
 * deliberate: "the top ten transitions" is what the table IS — a `LIMIT` the
 * card's own title states — not how it is ordered. Sorting the full link list
 * by From and then taking ten would leave a card titled "Top transitions"
 * showing the ten alphabetically-first ones, most of which are not top
 * anything. This sort orders the ten rows; it does not choose them.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const JOURNEY_TRANSITION_DEFAULT_SORT: SortState = { key: 'users', dir: 'desc' };

export function journeyTransitionAccessor(key: string): (l: JourneyLink) => SortValue {
  const accessor = ACCESSORS[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[journey-sort] no accessor for column "${key}" — sorting by ` +
        `"${JOURNEY_TRANSITION_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in ` +
        `journey-sort.ts.`,
    );
  }
  return ACCESSORS[JOURNEY_TRANSITION_DEFAULT_SORT.key];
}
