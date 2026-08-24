// Makes a `tr.clickable` row behave like the link it already acts as: the first
// cell carries a real `<a href>` (so right-click → "Open in new tab" and
// middle-click work at all), and the rest of the row honours the same
// modifier-click conventions instead of navigating out from under them.
//
// The branching lives in `lib/models/row-nav.ts`, which is testable; this file
// is only the DOM/router adapter. `rowHref` is re-exported so a call site needs
// one import for both halves.
import { push } from 'svelte-spa-router';
import { decideRowNav } from '../models/row-nav';

export { rowHref } from '../models/row-nav';
import { rowHref } from '../models/row-nav';

/** Interactive descendants that own their own plain click. */
const OWNS_CLICK = 'button, input, select, textarea, label, [role="button"]';

/**
 * Row-level click handler for a table row that navigates to `path`.
 *
 * Wire it to BOTH `onclick` and `onauxclick` — `click` fires only for the
 * primary button, so middle-click is invisible without the second one.
 */
export function rowNav(e: MouseEvent, path: string): void {
  const el = e.target instanceof Element ? e.target : null;
  const action = decideRowNav({
    button: e.button,
    ctrlKey: e.ctrlKey,
    metaKey: e.metaKey,
    shiftKey: e.shiftKey,
    overLink: !!el?.closest('a[href]'),
    overControl: !!el?.closest(OWNS_CLICK),
  });

  if (action === 'ignore') return;
  if (action === 'new-tab') {
    // Also suppresses middle-click autoscroll, which would otherwise start up
    // behind the new tab.
    e.preventDefault();
    window.open(rowHref(path), '_blank', 'noopener');
    return;
  }
  push(path);
}
