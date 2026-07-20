/**
 * `@sauron/browser` — public API surface.
 *
 * Error reporting + product analytics for the browser. Import the named
 * functions, or the `Sauron` facade / default export.
 *
 * ```ts
 * import { Sauron } from '@sauron/browser';
 * Sauron.init({ dsn: 'https://pk_test@localhost:8081/1', release: 'web@1.4.2' });
 * Sauron.track('checkout_completed', { cart_value: 42.5 });
 * ```
 */

import { addBreadcrumb as addBreadcrumbApi, type BreadcrumbInput } from './api/breadcrumbs.js';
import { captureException as captureExceptionApi, captureMessage as captureMessageApi } from './api/capture.js';
import {
  identify as identifyApi,
  setScreen as setScreenApi,
  track as trackApi,
  trackTransaction as trackTransactionApi,
  type TransactionInput,
} from './api/product.js';
import { getClient, init as initClient, SauronClient } from './client.js';
import { getScreen as getScreenApi } from './screen.js';
import type { Hint, InitOptions, Level, TrackOptions, UserInput } from './types.js';

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

/** Associate the session with a known user. */
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

/** Record a breadcrumb. */
export function addBreadcrumb(breadcrumb: BreadcrumbInput, hint?: Hint): void {
  addBreadcrumbApi(breadcrumb, hint);
}

/** Set (or clear, with `null`) the current user. */
export function setUser(user: UserInput): void {
  getClient()?.getScope().setUser(user);
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
  setTag,
  setTags,
  setContext,
  setExtra,
  setScreen,
  getScreen,
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
} from './types.js';
