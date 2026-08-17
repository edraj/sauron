/**
 * Presentation logic for `BreadcrumbTrail` rows — pure functions of a
 * `Breadcrumb`, kept out of the component so they can be tested without a DOM
 * (the dashboard's vitest runs on the node environment).
 */
import type { Breadcrumb } from './index';

/**
 * How a navigation moved the history stack. Both SDKs speak this vocabulary:
 * Flutter's `SauronNavigatorObserver` emits all four, the browser SDK emits
 * every one but `remove` (which has no web equivalent).
 *
 * On the web a forward step (`history.forward()`) arrives as `pop` — the
 * `popstate` event cannot be told apart from a back step — so `pop` means
 * "moved through history", not specifically "went back".
 */
export type NavigationOperation = 'push' | 'pop' | 'replace' | 'remove';

const OPERATIONS: readonly string[] = ['push', 'pop', 'replace', 'remove'];

/**
 * The direction of a navigation breadcrumb, or `null` when there isn't one to
 * show — a non-navigation crumb, an older SDK that predates `operation`, or a
 * value outside the vocabulary. Callers fall back to the level-coloured dot.
 */
export function navigationOperation(b: Breadcrumb): NavigationOperation | null {
  if (b.type !== 'navigation') return null;
  const op = b.data?.operation;
  if (typeof op !== 'string' || !OPERATIONS.includes(op)) return null;
  return op as NavigationOperation;
}

/**
 * The one-line body of a crumb: its message, else its data flattened to
 * `k: v` pairs, else the category it belongs to.
 *
 * `operation` is dropped from the flattening only when the node is rendering
 * it as a glyph — an unrecognised value still prints, so nothing is silently
 * swallowed.
 */
export function breadcrumbSummary(b: Breadcrumb): string {
  if (b.message) return b.message;
  if (b.data && typeof b.data === 'object') {
    const shown = navigationOperation(b) !== null ? 'operation' : null;
    const entries = Object.entries(b.data).filter(([k]) => k !== shown);
    if (entries.length) return entries.map(([k, v]) => `${k}: ${String(v)}`).join(', ');
  }
  return b.category ?? b.type;
}
