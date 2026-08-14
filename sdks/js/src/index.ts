/**
 * `@edraj/sauron-browser` — public API surface.
 *
 * Error reporting + product analytics for the browser. Import the named
 * functions, or the `Sauron` facade / default export.
 *
 * ```ts
 * import { Sauron } from '@edraj/sauron-browser';
 * Sauron.init({ dsn: 'https://pk_test@localhost:8081/1', release: 'web@1.4.2' });
 * Sauron.track('checkout_completed', { cart_value: 42.5 });
 * ```
 */

import { addBreadcrumb as addBreadcrumbApi, type BreadcrumbInput } from './api/breadcrumbs.js';
import { captureException as captureExceptionApi, captureMessage as captureMessageApi } from './api/capture.js';
import {
  cancelWorkflow as cancelWorkflowApi,
  endWorkflow as endWorkflowApi,
  identify as identifyApi,
  setScreen as setScreenApi,
  startWorkflow as startWorkflowApi,
  track as trackApi,
  trackTransaction as trackTransactionApi,
  type TransactionInput,
} from './api/product.js';
import { getClient, init as initClient, SauronClient } from './client.js';
import { getScreen as getScreenApi } from './screen.js';
import type {
  ActiveWorkflow,
  Hint,
  InitOptions,
  Level,
  TrackOptions,
  UserInput,
  WorkflowResult,
} from './types.js';
import { getWorkflow as getWorkflowApi } from './workflow.js';

/** Initialize the SDK. See {@link InitOptions}. */
export function init(options: InitOptions): SauronClient {
  return initClient(options);
}

/** Capture an exception (or any thrown value). */
export function captureException(err: unknown, hint?: Hint): void {
  captureExceptionApi(err, hint);
}

/** Capture a plain message at the given `level` (default `info`). */
export function captureMessage(message: string, level: Level = 'info', hint?: Hint): void {
  captureMessageApi(message, level, hint);
}

/** Record a product-analytics event, optionally with per-call tags/contexts/extra. */
export function track(
  name: string,
  properties?: Record<string, unknown>,
  options?: TrackOptions,
): void {
  trackApi(name, properties, options);
}

/**
 * Associate the session with a known user.
 *
 * The `anonymous_id` sent with the identify item is the current anon id — but
 * only when it was actually used as a `distinct_id` this session, and never
 * when it belongs to a different person than the last one who identified on
 * this device. In that case a fresh anon id is minted first and `null` is
 * sent instead, since the old one is already permanently bound to the
 * previous person server-side (see `reset()`).
 */
export function identify(id: string, traits?: Record<string, unknown>): void {
  identifyApi(id, traits);
}

/** Record a performance transaction (navigation, http, screen load, ...). */
export function trackTransaction(input: TransactionInput): void {
  trackTransactionApi(input);
}

/** Set the current screen (emits a `$screen` view on change). */
export function setScreen(name: string): void {
  setScreenApi(name);
}

/** The current screen name, or null. */
export function getScreen(): string | null {
  return getScreenApi();
}

/**
 * Start a named, explicitly-bounded workflow. `workflow_id` is a fresh
 * client-generated UUID; the id + name are then stamped on every subsequent
 * event/error/transaction until the workflow ends or is cancelled. Optional —
 * an app that never calls this behaves exactly as before.
 */
export function startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult {
  return startWorkflowApi(name, options);
}

/** End the active workflow (or the one named `name`, if given). */
export function endWorkflow(name?: string): WorkflowResult {
  return endWorkflowApi(name);
}

/** Cancel the active workflow (or the one named `name`, if given). */
export function cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult {
  return cancelWorkflowApi(name, options);
}

/** The currently active workflow, or `null` when none is active. */
export function getWorkflow(): ActiveWorkflow | null {
  return getWorkflowApi();
}

/** Record a breadcrumb. */
export function addBreadcrumb(breadcrumb: BreadcrumbInput, hint?: Hint): void {
  addBreadcrumbApi(breadcrumb, hint);
}

/**
 * Set (or clear, with `null`) the current user.
 *
 * `setUser(null)` is a logout, so it also calls `reset()` for you — otherwise
 * the next anonymous visitor on this browser inherits the previous person's
 * durable id and a later identify() aliases them together server-side.
 */
export function setUser(user: UserInput): void {
  if (user === null) {
    getClient()?.reset();
    return;
  }
  getClient()?.getScope().setUser(user);
}

/**
 * Forget the current person: clears the scope user, mints a fresh anonymous
 * id, forgets the last identified user, and rotates the session id so a
 * single session can never span two different people. Call this on logout.
 */
export function reset(): void {
  getClient()?.reset();
}

/** Set a single scope tag (lifted onto later errors/events). */
export function setTag(key: string, value: string): void {
  getClient()?.getScope().setTag(key, value);
}

/** Merge a batch of scope tags (last-write-wins per key). */
export function setTags(tags: Record<string, string>): void {
  getClient()?.getScope().setTags(tags);
}

/** Set (replace) a named scope context block. */
export function setContext(name: string, block: Record<string, unknown>): void {
  getClient()?.getScope().setContext(name, block);
}

/** Set a single freeform scope extra value. */
export function setExtra(key: string, value: unknown): void {
  getClient()?.getScope().setExtra(key, value);
}

/** Flush pending events. Resolves `false` if `timeoutMs` elapses first. */
export function flush(timeoutMs?: number): Promise<boolean> {
  const client = getClient();
  return client ? client.flush(timeoutMs) : Promise.resolve(false);
}

/** Flush and tear down the SDK, restoring all patched globals. */
export function close(timeoutMs?: number): Promise<boolean> {
  const client = getClient();
  return client ? client.close(timeoutMs) : Promise.resolve(false);
}

/** The active client instance, or `null` before `init`. */
export { getClient, SauronClient };

/** Grouped facade + default export. */
export const Sauron = {
  init,
  captureException,
  captureMessage,
  track,
  trackTransaction,
  identify,
  addBreadcrumb,
  setUser,
  reset,
  setTag,
  setTags,
  setContext,
  setExtra,
  setScreen,
  getScreen,
  startWorkflow,
  endWorkflow,
  cancelWorkflow,
  getWorkflow,
  flush,
  close,
  getClient,
};

export default Sauron;

/* ------------------------------------------------------------- re-exports */

export { parseDsn, DsnError } from './dsn.js';
export type { Dsn } from './dsn.js';
export { buildEnvelope } from './envelope.js';
export { parseStackString, parseError, isInAppFrame } from './stacktrace/parse.js';
export { SDK_NAME, SDK_VERSION } from './utils.js';
export type { BreadcrumbInput } from './api/breadcrumbs.js';
export type { TransactionInput } from './api/product.js';

export type {
  Level,
  ItemType,
  TransactionOp,
  Frame,
  Mechanism,
  ExceptionValue,
  Breadcrumb,
  ErrorItem,
  EventItem,
  IdentifyItem,
  BreadcrumbBatchItem,
  TransactionItem,
  EnvelopeItem,
  DeviceContext,
  OsContext,
  AppContext,
  RuntimeContext,
  UserContext,
  Context,
  SdkInfo,
  EnvelopeHeader,
  Envelope,
  Hint,
  UserInput,
  BeforeSend,
  BeforeBreadcrumb,
  TransportOptions,
  InitOptions,
  CaptureOptions,
  TrackOptions,
  ResolvedOptions,
  WorkflowStatus,
  WorkflowResult,
  ActiveWorkflow,
} from './types.js';
