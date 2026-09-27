import { captureException } from '../api/capture.js';
import { getClient } from '../client.js';
import { isWrapped, markWrapped, registerPatch } from './instrument.js';

/**
 * Install `window.onerror` and `window.onunhandledrejection`, chaining any
 * previously-registered handler so we never clobber the host app's own logging.
 */
export function installGlobalHandlers(): void {
  const win = globalThis as typeof globalThis & {
    onerror?: OnErrorEventHandler;
    onunhandledrejection?: ((event: PromiseRejectionEvent) => unknown) | null;
  };

  installOnError(win);
  installOnUnhandledRejection(win);
}

/**
 * Capture what `window.onerror` reports. Also how the script-tag build replays
 * an error the GTM loader queued before the SDK loaded (`src/global.ts`).
 */
export function captureOnError(
  message: Event | string,
  source?: string,
  lineno?: number,
  colno?: number,
  error?: unknown,
): void {
  if (!getClient()) return;
  const err = error ?? syntheticError(message, source, lineno, colno);
  captureException(err, { mechanism: { type: 'onerror', handled: false }, level: 'error' });
}

/** Capture an unhandled rejection's reason; the counterpart of {@link captureOnError}. */
export function captureRejection(reason: unknown): void {
  if (!getClient()) return;
  captureException(reason, {
    mechanism: { type: 'onunhandledrejection', handled: false },
    level: 'error',
  });
}

function installOnError(win: typeof globalThis & { onerror?: OnErrorEventHandler }): void {
  const previous = win.onerror;
  if (isWrapped(previous)) return;

  const handler = markWrapped(function sauronOnError(
    this: unknown,
    message: Event | string,
    source?: string,
    lineno?: number,
    colno?: number,
    error?: unknown,
  ): boolean {
    captureOnError(message, source, lineno, colno, error);
    if (typeof previous === 'function') {
      return Boolean(
        previous.call(this, message, source, lineno, colno, error as Error | undefined),
      );
    }
    return false;
  });

  win.onerror = handler as OnErrorEventHandler;
  registerPatch('onerror', () => {
    win.onerror = (previous ?? null) as OnErrorEventHandler;
  });
}

function installOnUnhandledRejection(
  win: typeof globalThis & {
    onunhandledrejection?: ((event: PromiseRejectionEvent) => unknown) | null;
  },
): void {
  const previous = win.onunhandledrejection;
  if (isWrapped(previous)) return;

  const handler = markWrapped(function sauronOnRejection(
    this: unknown,
    event: PromiseRejectionEvent,
  ): unknown {
    captureRejection(event && typeof event === 'object' && 'reason' in event ? event.reason : event);
    if (typeof previous === 'function') {
      return previous.call(this, event);
    }
    return undefined;
  });

  win.onunhandledrejection = handler as (event: PromiseRejectionEvent) => unknown;
  registerPatch('onunhandledrejection', () => {
    win.onunhandledrejection = previous ?? null;
  });
}

/** Build an Error carrying a best-effort stack from `onerror` positional args. */
function syntheticError(
  message: Event | string,
  source?: string,
  lineno?: number,
  colno?: number,
): Error {
  const msg = typeof message === 'string' ? message : 'Unknown error';
  const err = new Error(msg);
  if (source) {
    err.stack = `Error: ${msg}\n    at ${source}:${lineno ?? 0}:${colno ?? 0}`;
  }
  return err;
}
