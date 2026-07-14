import type { Transport } from './transport.js';

/**
 * Wire up the page-unload flush path. When the page is hidden or unloaded there
 * is no time for an async `fetch` round-trip, so we hand the pending batch to
 * `navigator.sendBeacon` (see {@link Transport.flushToBeacon}).
 *
 * Returns a cleanup function that removes the listeners.
 */
export function installBeacon(transport: Transport): () => void {
  const win = globalThis as {
    addEventListener?: (t: string, h: () => void) => void;
    removeEventListener?: (t: string, h: () => void) => void;
  };
  const doc = (globalThis as { document?: Document }).document;

  const flush = (): void => {
    try {
      transport.flushToBeacon();
    } catch {
      /* never throw from an unload handler */
    }
  };

  const onVisibility = (): void => {
    if (doc && doc.visibilityState === 'hidden') flush();
  };

  if (doc && typeof doc.addEventListener === 'function') {
    doc.addEventListener('visibilitychange', onVisibility);
  }
  if (typeof win.addEventListener === 'function') {
    win.addEventListener('pagehide', flush);
  }

  return () => {
    if (doc && typeof doc.removeEventListener === 'function') {
      doc.removeEventListener('visibilitychange', onVisibility);
    }
    if (typeof win.removeEventListener === 'function') {
      win.removeEventListener('pagehide', flush);
    }
  };
}
