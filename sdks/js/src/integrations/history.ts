import { addNavigationBreadcrumb } from '../api/breadcrumbs.js';
import { isInternal, isWrapped, markWrapped, registerPatch, withInternal } from './instrument.js';

let navHandler: ((path: string) => void) | null = null;

/** Register (or clear) a callback fired with the new path on each SPA navigation. */
export function onNavigation(cb: ((path: string) => void) | null): void {
  navHandler = cb;
}

/** Reduce a URL to its path portion (relative to the current origin). */
function toPath(url: string | null, base: string | undefined): string | null {
  if (url === null || url === undefined) return url;
  try {
    const parsed = new URL(url, base);
    return parsed.pathname + parsed.search + parsed.hash;
  } catch {
    return url;
  }
}

/**
 * Patch `history.pushState`/`replaceState` and listen for `popstate` to emit
 * `{from, to}` navigation breadcrumbs for SPA route changes.
 */
export function installHistory(): void {
  const g = globalThis as typeof globalThis & {
    history?: History;
    location?: Location;
    addEventListener?: (t: string, h: () => void) => void;
    removeEventListener?: (t: string, h: () => void) => void;
  };
  const hist = g.history;
  const loc = g.location;
  if (!hist) return;

  let lastPath = toPath(loc?.href ?? null, loc?.href);

  const emit = (toHref: string | null): void => {
    const to = toPath(toHref, loc?.href);
    const from = lastPath;
    lastPath = to;
    if (from === to) return;
    withInternal(() => addNavigationBreadcrumb(from, to));
    if (to && navHandler) {
      try {
        navHandler(to);
      } catch {
        /* never let screen tracking break navigation */
      }
    }
  };

  const wrap = (name: 'pushState' | 'replaceState'): void => {
    const original = hist[name];
    if (typeof original !== 'function' || isWrapped(original)) return;

    hist[name] = markWrapped(function sauronHistory(this: History, ...args: unknown[]) {
      const result = (original as (...a: unknown[]) => unknown).apply(this, args);
      if (!isInternal()) {
        const urlArg = args[2];
        emit(urlArg != null ? String(urlArg) : (loc?.href ?? null));
      }
      return result;
    }) as History[typeof name];

    registerPatch(`history.${name}`, () => {
      hist[name] = original;
    });
  };

  wrap('pushState');
  wrap('replaceState');

  if (typeof g.addEventListener === 'function') {
    const onPopState = (): void => {
      if (!isInternal()) emit(loc?.href ?? null);
    };
    g.addEventListener('popstate', onPopState);
    registerPatch('popstate', () => {
      g.removeEventListener?.('popstate', onPopState);
    });
  }
}
