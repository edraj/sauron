/**
 * Entry point of the script-tag builds — `dist/sauron.min.js` and
 * `dist/sauron.es5.min.js`, built by `scripts/build-global.mjs`. Not part of
 * the npm entry points.
 *
 * Exposes the whole public API as `window.Sauron`, for pages that can't import
 * a module: a Google Tag Manager Custom HTML tag, a CMS footer, a plain
 * `<script src>`.
 *
 * The README's GTM loader snippet puts a stand-in `window.Sauron` in place
 * before this file arrives: the same method names, each pushing
 * `[name, args]` onto `Sauron.q`. It also queues the page's uncaught errors
 * and unhandled rejections, as `['$error', [event]]` and
 * `['$unhandledrejection', [event]]`. On load the real API takes its place and
 * the queue is replayed.
 */

import * as api from './index.js';
import { captureOnError, captureRejection } from './integrations/globalHandlers.js';

const root = self as unknown as { Sauron?: { init?: unknown; q?: unknown } };
const existing = root.Sauron;

if (existing && Array.isArray(existing.q)) {
  const queued: unknown[] = existing.q;
  root.Sauron = api;
  // init() first: before it, every other call is a no-op.
  queued.filter(isInit).concat(queued.filter((entry) => !isInit(entry))).forEach(run);
  // A tag that kept a reference to the stand-in still reaches the SDK. Errors
  // no longer come this way: from here on the SDK's own handlers see them.
  existing.q = {
    push: (entry: unknown) => {
      if (!isEarlyError(entry)) run(entry);
    },
  };
} else if (typeof existing?.init !== 'function') {
  root.Sauron = api;
}
// Otherwise a copy is already loaded (a tag that fires on every history
// change, say): keep it. It owns the installed client and every patched
// global, and calling init() on it again tears the old client down cleanly — a
// second copy would install a second set of patches on top of the first.

function isInit(entry: unknown): boolean {
  return Array.isArray(entry) && entry[0] === 'init';
}

function isEarlyError(entry: unknown): boolean {
  return Array.isArray(entry) && (entry[0] === '$error' || entry[0] === '$unhandledrejection');
}

/**
 * Make a queued call as the page would have, had the SDK been loaded — or
 * capture a queued page error as the SDK's own handlers would have.
 */
function run(entry: unknown): void {
  if (!Array.isArray(entry) || typeof entry[0] !== 'string') return;
  const name: string = entry[0];
  const args: unknown[] = Array.isArray(entry[1]) ? entry[1] : [];
  try {
    if (name === '$error') {
      const event = args[0] as ErrorEvent;
      captureOnError(event.message, event.filename, event.lineno, event.colno, event.error);
    } else if (name === '$unhandledrejection') {
      captureRejection((args[0] as PromiseRejectionEvent).reason);
    } else {
      const fn = (api as unknown as Record<string, unknown>)[name];
      if (typeof fn === 'function') fn(...args);
    }
  } catch (err) {
    // Surface it the way a direct call would have, as an uncaught error,
    // without dropping the calls queued behind it.
    setTimeout(() => {
      throw err;
    });
  }
}
