/**
 * Instrumentation primitives shared by every integration:
 *
 *  - a reentrancy guard so SDK-internal work never triggers its own breadcrumbs
 *    (e.g. the transport's own `fetch` must not create a "fetch" breadcrumb that
 *    schedules another send — an infinite loop),
 *  - a DSN-host denylist (belt-and-suspenders alongside the guard),
 *  - an idempotent "already wrapped" tag so double-`init` doesn't stack wrappers,
 *  - a patch registry so `close()` can restore every original.
 */

const WRAPPED = '__sauron_wrapped__';

let internalDepth = 0;

/** Enter SDK-internal execution (breadcrumb capture is suppressed while > 0). */
export function beginInternal(): void {
  internalDepth++;
}

/** Leave SDK-internal execution. */
export function endInternal(): void {
  internalDepth = Math.max(0, internalDepth - 1);
}

/** Run `fn` with the reentrancy guard held (synchronous scope only). */
export function withInternal<T>(fn: () => T): T {
  beginInternal();
  try {
    return fn();
  } finally {
    endInternal();
  }
}

/** True while SDK-internal code is running. */
export function isInternal(): boolean {
  return internalDepth > 0;
}

let dsnHost: string | null = null;

/** Register the DSN host (`host:port`) used for the request denylist. */
export function setDsnHost(host: string | null): void {
  dsnHost = host;
}

/** True when `rawUrl` targets the DSN host — such requests must be ignored. */
export function isDsnRequest(rawUrl: string): boolean {
  if (!dsnHost || !rawUrl) return false;
  try {
    const base = (globalThis as { location?: { href?: string } }).location?.href;
    const u = new URL(rawUrl, base);
    return u.host === dsnHost;
  } catch {
    return false;
  }
}

/** Tag a wrapper function so we never wrap it twice. */
export function markWrapped<T>(fn: T): T {
  try {
    (fn as Record<string, unknown>)[WRAPPED] = true;
  } catch {
    /* frozen function — best effort */
  }
  return fn;
}

/** True when `fn` was produced by {@link markWrapped}. */
export function isWrapped(fn: unknown): boolean {
  if (fn === null || (typeof fn !== 'function' && typeof fn !== 'object')) return false;
  return (fn as Record<string, unknown>)[WRAPPED] === true;
}

type Unpatch = () => void;

const patches: Array<{ name: string; unpatch: Unpatch }> = [];

/** Register an undo function to be run by {@link unpatchAll}. */
export function registerPatch(name: string, unpatch: Unpatch): void {
  patches.push({ name, unpatch });
}

/** Restore every patched global to its original, in reverse order. */
export function unpatchAll(): void {
  while (patches.length) {
    const p = patches.pop();
    try {
      p?.unpatch();
    } catch {
      /* ignore restore failures */
    }
  }
}
