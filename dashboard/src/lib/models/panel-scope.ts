/**
 * Copy for a panel whose request carries LESS of the page's query than the list
 * next to it does.
 *
 * ## The defect this exists to close
 *
 * Issues and Events both render an overview panel above a filtered list. The
 * overview is fetched from its own endpoint, and those endpoints take a date
 * range and nothing else — no `filter`, no `q`. So the two numbers on screen
 * describe different sets, and until this module nothing said so:
 *
 *   - `GET /v1/apps/{id}/issues/stats` -> the seven Issues tiles. Takes
 *     `since_days`, and `repo::issue_stats` does not even use it: the counts
 *     have NO date predicate at all. Issues also applies `status:unresolved`
 *     by default, so a first load puts `Total 5,000` in a tile roughly 200px
 *     above a pager reading `412 issues`.
 *   - the `series` half of that same payload -> the Occurrences chart, which
 *     DOES honour `since_days`, but no more of the query than the tiles do.
 *   - `GET /v1/apps/{id}/events/top` -> Events' Top events list. `since_days`
 *     and `limit`.
 *   - `GET /v1/apps/{id}/events/series` -> Events' volume chart. `since_days`
 *     and `name`, which is the one place a filter chip reaches an overview
 *     panel: the page passes the `name:eq` chip through as `name`.
 *
 * Passing the predicate to those endpoints was considered and rejected: with
 * the default `status:unresolved` chip applied, a filtered `Unresolved` tile
 * would equal `Total` and every other tile would read 0. The panels are a
 * broader view on purpose. They simply never said so.
 *
 * ## Why the copy describes the DIFFERENCE and not the panel's scope
 *
 * "App-wide" was the obvious label and it is not true. Every one of those four
 * routes is app-scoped *and* environment-scoped — they sit under
 * `/v1/apps/{app_id}/...` with no entry in `api/scope.ts`'s exclusion list, so
 * the axios interceptor attaches `environment_id`, and
 * `sessionStore.currentEnvId` is non-null by default. Three of the four narrow
 * on the date range as well.
 *
 * So a caption naming an absolute scope would have to name the environment and
 * the range to stay honest, and it would be naming a scope the list below
 * shares anyway — the environment switcher is in the Topbar and moves every
 * panel on the page together. The mismatch is never the environment. It is only
 * ever the part of the query the panel's request did not carry, so that is what
 * these sentences name, and they claim nothing else.
 */

/**
 * Which of the page's query controls a panel's own request did NOT carry.
 *
 * Written from the panel's point of view, not the bar's: `ignoresDateRange` is
 * false on a panel that takes `since_days`, whatever the picker is set to. A
 * caller should also report `false` when a control is at a setting that narrows
 * nothing (Issues' "All" range), since a control that is not narrowing the list
 * cannot be making the two disagree.
 */
export interface PanelScope {
  /**
   * How many filter chips are applied that this panel's request left out.
   *
   * A count rather than a boolean so the sentence can say "filter" or
   * "filters" — with one chip up, "The filters don't apply" reads as a claim
   * about a set the reader cannot see.
   */
  ignoredFilters: number;
  /** A free-text search is applied to the list and this panel left it out. */
  ignoresSearch: boolean;
  /** The date range narrows the list and this panel left it out. */
  ignoresDateRange: boolean;
  /**
   * Chip label of the filter this panel DOES apply, when it applies one and
   * ignores the rest — Events' volume chart takes the `name:eq` chip through
   * the `name` parameter and nothing else.
   *
   * Present so that panel does not get "The filters don't apply to this
   * chart." while one of them demonstrably does.
   */
  appliedFilterLabel?: string | null;
}

/** One named control, with the grammatical number the verb has to agree with. */
interface Control {
  text: string;
  plural: boolean;
}

/**
 * The ignored controls, in the order they read best — which is also the order
 * they appear in the FilterBar, left to right.
 */
function ignoredControls(scope: PanelScope): Control[] {
  const out: Control[] = [];
  if (scope.ignoredFilters > 0) {
    out.push({
      text: scope.ignoredFilters === 1 ? 'filter' : 'filters',
      plural: scope.ignoredFilters > 1,
    });
  }
  if (scope.ignoresSearch) out.push({ text: 'search', plural: false });
  if (scope.ignoresDateRange) out.push({ text: 'date range', plural: false });
  return out;
}

/** "filters", "filters and search", "filters, search and date range". */
function joinControls(items: Control[]): string {
  const words = items.map((i) => i.text);
  if (words.length <= 1) return words.join('');
  return `${words.slice(0, -1).join(', ')} and ${words[words.length - 1]}`;
}

/**
 * The caption for a panel, or `null` when the panel and the list agree and
 * there is nothing worth saying.
 *
 * `null` is the common case on a freshly loaded Events page and it is the
 * point: the caption is meant to be read when a filter is up, so it says
 * nothing when one is not. Callers render it into a line whose height is
 * reserved either way — text that pushes the page down when it arrives is a
 * layout jump on every chip added or removed.
 *
 * @param subject the panel as a noun phrase the sentence can end on —
 *   "these totals", "this chart", "this list". Takes no verb, so it needs no
 *   agreement with one.
 */
export function panelScopeNote(scope: PanelScope, subject: string): string | null {
  const ignored = ignoredControls(scope);
  if (ignored.length === 0) return null;

  // The positive form, for a panel that applies one chip and drops the rest.
  // Gated on there being something filter-shaped to contrast it against: with
  // only the date range ignored, "Only the Event filter applies" would answer a
  // question nobody asked and bury the one fact that matters.
  const label = scope.appliedFilterLabel;
  if (label && (scope.ignoredFilters > 0 || scope.ignoresSearch)) {
    return scope.ignoresDateRange
      ? `Only the ${label} filter applies to ${subject} — the date range doesn't.`
      : `Only the ${label} filter applies to ${subject}.`;
  }

  // "don't" for a list, or for a lone plural noun ("filters"); "doesn't" for a
  // lone singular one ("filter", "search", "date range").
  const plural = ignored.length > 1 || ignored[0].plural;
  return `The ${joinControls(ignored)} ${plural ? "don't" : "doesn't"} apply to ${subject}.`;
}
