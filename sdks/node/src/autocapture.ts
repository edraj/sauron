/**
 * Opt-in process-level hooks: auto-capture of uncaught errors and graceful
 * shutdown. Both are OFF by default (see {@link InitOptions.autoCaptureUnhandled}
 * / {@link InitOptions.autoShutdown}) and only installed when the consumer opts
 * in.
 *
 * Auto-capture never *swallows* a crash. An uncaught exception is captured with
 * `mechanism.handled = false`, the batch is flushed, and Node's default
 * behavior is preserved: if this SDK is the *sole* `uncaughtException` handler,
 * the process still exits non-zero (as Node would with no handler at all);
 * if another handler is registered, that handler decides the process's fate.
 * Unhandled rejections are captured but never terminate the process on their
 * own — Node's own `unhandledRejection` mode still governs that.
 */

import type { ProcessLike } from './types.js';
// Type-only import — erased at compile time, so there is no runtime import
// cycle with `client.ts` (which imports the functions below).
import type { SauronClient } from './client.js';

export interface AutoCaptureOptions {
  /** Injected process (tests). Defaults to the real Node `process`. */
  process?: ProcessLike;
}

type Uninstaller = () => void;

/** Conventional exit code for a terminating signal (`128 + signal number`). */
const SIGNAL_EXIT_CODE: Record<string, number> = { SIGINT: 130, SIGTERM: 143 };

const autoCaptureInstalled = new WeakMap<object, Uninstaller>();
const shutdownInstalled = new WeakMap<object, Uninstaller>();

function realProcess(): ProcessLike {
  return process as unknown as ProcessLike;
}

/**
 * Register `uncaughtException` + `unhandledRejection` handlers that capture with
 * `mechanism.handled = false`. Idempotent per client; returns an uninstaller
 * that removes the listeners.
 */
export function installAutoCapture(
  client: SauronClient,
  options: AutoCaptureOptions = {},
): Uninstaller {
  const existing = autoCaptureInstalled.get(client);
  if (existing) return existing;

  const proc = options.process ?? realProcess();
  // Guard against a capture path itself throwing and re-entering the handler.
  let capturing = false;

  const onUncaught = (error: unknown): void => {
    if (capturing) return;
    capturing = true;
    try {
      client.captureException(error, { level: 'fatal', handled: false });
    } catch {
      // Never let a capture failure mask the original crash.
    }
    capturing = false;
    void client.flush().then(exitIfSole, exitIfSole);
  };

  const exitIfSole = (): void => {
    const others = proc
      .listeners('uncaughtException')
      .filter((listener) => listener !== onUncaught);
    if (others.length === 0) proc.exit(1);
  };

  const onRejection = (reason: unknown): void => {
    if (capturing) return;
    capturing = true;
    try {
      client.captureException(reason, { level: 'error', handled: false });
    } catch {
      // ignore — see above.
    }
    capturing = false;
    void client.flush();
  };

  proc.on('uncaughtException', onUncaught);
  proc.on('unhandledRejection', onRejection);

  const uninstall: Uninstaller = () => {
    proc.removeListener('uncaughtException', onUncaught);
    proc.removeListener('unhandledRejection', onRejection);
    autoCaptureInstalled.delete(client);
  };
  autoCaptureInstalled.set(client, uninstall);
  return uninstall;
}

/**
 * Wire `beforeExit`/`SIGTERM`/`SIGINT` to `client.close()` for a graceful flush
 * on shutdown. `beforeExit` (event loop drained) just closes; a terminating
 * signal closes then exits with the conventional code so the SDK does not hang
 * the process. Idempotent per client; returns an uninstaller.
 */
export function installShutdownHooks(
  client: SauronClient,
  options: AutoCaptureOptions = {},
): Uninstaller {
  const existing = shutdownInstalled.get(client);
  if (existing) return existing;

  const proc = options.process ?? realProcess();
  let closing = false;

  const onBeforeExit = (): void => {
    if (closing) return;
    closing = true;
    void client.close();
  };

  const makeSignalHandler = (signal: string) => (): void => {
    if (closing) return;
    closing = true;
    const code = SIGNAL_EXIT_CODE[signal] ?? 0;
    void client.close().then(
      () => proc.exit(code),
      () => proc.exit(code),
    );
  };
  const onSigterm = makeSignalHandler('SIGTERM');
  const onSigint = makeSignalHandler('SIGINT');

  proc.on('beforeExit', onBeforeExit);
  proc.on('SIGTERM', onSigterm);
  proc.on('SIGINT', onSigint);

  const uninstall: Uninstaller = () => {
    proc.removeListener('beforeExit', onBeforeExit);
    proc.removeListener('SIGTERM', onSigterm);
    proc.removeListener('SIGINT', onSigint);
    shutdownInstalled.delete(client);
  };
  shutdownInstalled.set(client, uninstall);
  return uninstall;
}
