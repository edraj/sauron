// The decision behind a clickable table row: given a mouse event on the row,
// navigate in place, open a new tab, or keep out of the way.
//
// Pure and router-free on purpose. The unit suite runs in node, and
// `svelte-spa-router` publishes no node export condition — anything importing
// it cannot be unit-tested at all, so the branches live here and the four-line
// DOM adapter lives in `lib/utils/row-link.ts`.

/**
 * The in-app href for a router path.
 *
 * The dashboard is hash-routed, so a link to a route is `#` + the path `push()`
 * would take. This is the single place that knows that, which is what keeps a
 * row's first-cell `<a href>` and its click handler pointing at one destination
 * rather than two that drift apart.
 */
export function rowHref(path: string): string {
  return '#' + path;
}

/** What a mouse event on a navigable row should do. */
export type RowNavAction = 'ignore' | 'navigate' | 'new-tab';

/** The parts of a MouseEvent, plus a hit test, the decision turns on. */
export interface RowNavInput {
  /** MouseEvent.button: 0 primary, 1 middle, 2 secondary. */
  button: number;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  /** The event originated inside an `<a href>`. */
  overLink?: boolean;
  /** The event originated inside a non-link control (button, input, …). */
  overControl?: boolean;
}

export function decideRowNav(e: RowNavInput): RowNavAction {
  const middle = e.button === 1;
  // Right-click belongs to the context menu and nothing else. The button has to
  // be read BEFORE the modifiers because `auxclick` covers middle AND right:
  // checking `ctrlKey` first would open a tab behind the menu.
  if (!middle && e.button !== 0) return 'ignore';

  // An `<a>` under the cursor has its own destination and its own new-tab
  // behaviour. Never stack ours on top: on a middle-click that opens two tabs,
  // and one of them goes somewhere the user did not aim at. Rows here nest
  // links to other records — a session row links out to its person and its
  // device — so this is the common case, not the corner.
  if (e.overLink) return 'ignore';

  if (middle || e.ctrlKey || e.metaKey || e.shiftKey) return 'new-tab';

  // A plain click on any other control belongs to that control. The ones in
  // these rows today (TimeValue's relative/absolute toggle) already stop
  // propagation themselves, so this never fires — it is here so the next
  // control added to a row does not have to remember to. Deliberately checked
  // AFTER the modifier branch: unlike an anchor a button is not a destination,
  // so there is no competing tab to double up on, and middle-clicking one does
  // nothing at all today.
  if (e.overControl) return 'ignore';

  return 'navigate';
}
