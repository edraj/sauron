/**
 * Performance auto-instrumentation. In a browser this captures:
 *
 *  - one `navigation` transaction for the initial page load (navigation timing),
 *  - one `http` transaction per instrumented `fetch` call,
 *  - a `navigation` transaction on each SPA route change (History API).
 *
 * Everything here is strictly best-effort: instrumentation errors are swallowed
 * so an app `fetch` never breaks, and the original fetch rejection is re-thrown
 * unchanged to the caller. Opt in with `init({ performance: true })`.
 */

import { trackTransaction } from '../api/product.js';
import { isDsnRequest, isInternal, registerPatch } from './instrument.js';

/** Dedicated markers so we layer on top of the breadcrumb integrations without
 * colliding with their generic "wrapped" tag. */
const PERF_FETCH = '__sauron_perf_fetch__';
const PERF_HISTORY = '__sauron_perf_history__';

interface GlobalLike {
  fetch?: typeof fetch;
  history?: History;
  location?: Location;
  document?: Document;
  performance?: Performance;
  requestAnimationFrame?: (cb: () => void) => number;
  addEventListener?: (type: string, handler: () => void) => void;
  removeEventListener?: (type: string, handler: () => void) => void;
}

function g(): GlobalLike {
  return globalThis as GlobalLike;
}

/** Tag a wrapper function with a marker so we never wrap it twice. */
function mark(fn: object, key: string): void {
  (fn as unknown as Record<string, unknown>)[key] = true;
}

/** True when `fn` was tagged with {@link mark}. */
function isMarked(fn: unknown, key: string): boolean {
  return typeof fn === 'function' && (fn as unknown as Record<string, unknown>)[key] === true;
}

/** High-resolution clock, degrading to `Date.now()` when `performance` is absent. */
function clock(): () => number {
  const perf = g().performance;
  if (perf && typeof perf.now === 'function') return () => perf.now();
  return () => Date.now();
}

function resolveUrl(input: unknown): string {
  if (typeof input === 'string') return input;
  if (input && typeof input === 'object') {
    const withUrl = input as { url?: string; href?: string };
    if (typeof withUrl.url === 'string') return withUrl.url;
    if (typeof withUrl.href === 'string') return withUrl.href;
  }
  return String(input);
}

function resolveMethod(input: unknown, init: unknown): string {
  const fromInit = (init as { method?: string } | undefined)?.method;
  const fromReq = (input as { method?: string } | null)?.method;
  return String(fromInit ?? fromReq ?? 'GET').toUpperCase();
}

/** Reduce a URL to its path portion (relative to the current origin). */
function pathOf(url: string | null | undefined): string {
  if (!url) return '/';
  try {
    const base = g().location?.href;
    return new URL(url, base).pathname;
  } catch {
    return url;
  }
}

/* ------------------------------------------------------- navigation timing */

/** Prefer the modern `PerformanceNavigationTiming.duration`, else the legacy
 * `timing.loadEventEnd - navigationStart`. Returns `null` when unavailable. */
function navigationDuration(perf: Performance): number | null {
  try {
    const entries = perf.getEntriesByType?.('navigation') as
      | PerformanceNavigationTiming[]
      | undefined;
    if (entries && entries.length > 0) {
      const d = entries[0].duration;
      if (typeof d === 'number' && d > 0) return d;
    }
  } catch {
    /* getEntriesByType unsupported — fall through */
  }
  const timing = (perf as { timing?: PerformanceTiming }).timing;
  if (timing && timing.loadEventEnd && timing.navigationStart) {
    const d = timing.loadEventEnd - timing.navigationStart;
    if (d > 0) return d;
  }
  return null;
}

function installNavigationTiming(): void {
  const perf = g().performance;
  if (!perf) return;

  const capture = (): void => {
    try {
      const durationMs = navigationDuration(perf);
      if (durationMs === null) return;
      trackTransaction({ name: pathOf(g().location?.pathname), op: 'navigation', durationMs });
    } catch {
      /* swallow */
    }
  };

  // Timing is only final once `load` has fired; capture now if it already has.
  const doc = g().document;
  if (doc && doc.readyState === 'complete') {
    capture();
  } else if (typeof g().addEventListener === 'function') {
    const onLoad = (): void => {
      capture();
      g().removeEventListener?.('load', onLoad);
    };
    g().addEventListener?.('load', onLoad);
    registerPatch('perf.load', () => g().removeEventListener?.('load', onLoad));
  } else {
    capture();
  }
}

/* -------------------------------------------------------------- fetch timing */

function emitHttp(
  method: string,
  url: string,
  start: number,
  end: number,
  status: number | null,
  ok: boolean,
): void {
  try {
    trackTransaction({
      name: `${method} ${pathOf(url)}`,
      op: 'http',
      durationMs: Math.max(0, end - start),
      status: ok ? 'ok' : 'error',
      httpMethod: method,
      httpStatus: status ?? undefined,
      url,
    });
  } catch {
    /* never throw into user code */
  }
}

function installFetchTiming(): void {
  const original = g().fetch;
  if (typeof original !== 'function') return;
  if (isMarked(original, PERF_FETCH)) return;

  const now = clock();

  const wrapped = function sauronPerfFetch(
    this: unknown,
    input: RequestInfo | URL,
    init?: RequestInit,
  ): Promise<Response> {
    let url = '';
    let method = 'GET';
    let record = false;
    try {
      url = resolveUrl(input);
      method = resolveMethod(input, init);
      record = !isInternal() && !isDsnRequest(url);
    } catch {
      record = false;
    }

    const start = record ? now() : 0;
    // Return the ORIGINAL promise so the caller sees the untouched result and
    // any rejection propagates unchanged.
    const promise = original.call(this, input as RequestInfo, init);

    if (record) {
      promise.then(
        (res) => emitHttp(method, url, start, now(), res?.status ?? null, res?.ok ?? false),
        () => emitHttp(method, url, start, now(), null, false),
      );
    }
    return promise;
  };

  mark(wrapped, PERF_FETCH);
  g().fetch = wrapped as typeof fetch;
  registerPatch('perf.fetch', () => {
    g().fetch = original;
  });
}

/* ------------------------------------------------------------ history timing */

function installHistoryTiming(): void {
  const hist = g().history;
  if (!hist) return;
  const now = clock();

  const emit = (): void => {
    try {
      const path = pathOf(g().location?.pathname);
      const start = now();
      const finish = (): void => {
        try {
          trackTransaction({ name: path, op: 'navigation', durationMs: Math.max(0, now() - start) });
        } catch {
          /* swallow */
        }
      };
      // Approximate the client-side render span with a single animation frame.
      const raf = g().requestAnimationFrame;
      if (typeof raf === 'function') raf(finish);
      else finish();
    } catch {
      /* swallow */
    }
  };

  const wrap = (name: 'pushState' | 'replaceState'): void => {
    const original = hist[name];
    if (typeof original !== 'function') return;
    if (isMarked(original, PERF_HISTORY)) return;

    const patched = function sauronPerfHistory(this: History, ...args: unknown[]) {
      const result = (original as (...a: unknown[]) => unknown).apply(this, args);
      if (!isInternal()) emit();
      return result;
    };
    mark(patched, PERF_HISTORY);
    hist[name] = patched as History[typeof name];
    registerPatch(`perf.history.${name}`, () => {
      hist[name] = original;
    });
  };

  wrap('pushState');
  wrap('replaceState');

  if (typeof g().addEventListener === 'function') {
    const onPopState = (): void => {
      if (!isInternal()) emit();
    };
    g().addEventListener?.('popstate', onPopState);
    registerPatch('perf.popstate', () => g().removeEventListener?.('popstate', onPopState));
  }
}

/* --------------------------------------------------------------------- entry */

/** Install performance auto-capture. No-op outside a browser-like environment. */
export function installPerformance(): void {
  if (typeof g().document === 'undefined') return;
  installNavigationTiming();
  installFetchTiming();
  installHistoryTiming();
}
