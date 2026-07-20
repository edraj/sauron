/**
 * @sauron/node — server-side Node/TypeScript SDK.
 *
 * Dispatches product-analytics events and captured exceptions to the Sauron
 * ingest gateway. No browser/DOM/auto-instrumentation — a buffered background
 * HTTP transport over Node's global `fetch`.
 */

import { SauronClient } from './client.js';
import { getCurrentScope } from './scope.js';
import type {
  BreadcrumbInput,
  CaptureExceptionOptions,
  InitOptions,
  Level,
  MetadataOptions,
  TransactionInput,
  User,
} from './types.js';

export { SauronClient, describeError } from './client.js';
export { parseDsn, DsnError } from './dsn.js';
export type { Dsn } from './dsn.js';
export { parseStackString, parseError, isInAppFrame } from './stacktrace.js';
export { Transport } from './transport.js';
export {
  Scope,
  getGlobalScope,
  getCurrentScope,
  withScope,
  runWithAsyncScope,
  configureScope,
} from './scope.js';
export { installAutoCapture, installShutdownHooks } from './autocapture.js';
export type { AutoCaptureOptions } from './autocapture.js';
export type * from './types.js';

let activeClient: SauronClient | null = null;

/**
 * Initialize the global Sauron client. Returns the client for direct use.
 * A clearly-invalid DSN throws a typed `DsnError`.
 */
export function init(options: InitOptions): SauronClient {
  activeClient = new SauronClient(options);
  return activeClient;
}

/** The client created by the most recent {@link init}, if any. */
export function getClient(): SauronClient | null {
  return activeClient;
}

/** Capture a product-analytics event. No-op if the SDK is not initialized. */
export function track(
  event: string,
  distinctId: string,
  properties?: Record<string, unknown>,
  options?: MetadataOptions,
): void {
  activeClient?.track(event, distinctId, properties, options);
}

/** Capture a native `Error`. No-op if the SDK is not initialized. */
export function captureException(error: unknown, options?: CaptureExceptionOptions): void {
  activeClient?.captureException(error, options);
}

/** Capture a bare message. No-op if the SDK is not initialized. */
export function captureMessage(message: string, level?: Level, options?: MetadataOptions): void {
  activeClient?.captureMessage(message, level, options);
}

/** Associate traits with a distinct id. No-op if the SDK is not initialized. */
export function identify(distinctId: string, traits?: Record<string, unknown>): void {
  activeClient?.identify(distinctId, traits);
}

/**
 * Emit a performance transaction. No-op if the SDK is not initialized.
 * `distinct_id` falls back to the scoped user's id when omitted.
 */
export function trackTransaction(input: TransactionInput): void {
  activeClient?.trackTransaction(input);
}

/**
 * Add a breadcrumb to the active scope (runs `beforeBreadcrumb`). No-op if the
 * SDK is not initialized.
 */
export function addBreadcrumb(crumb: BreadcrumbInput): void {
  activeClient?.addBreadcrumb(crumb);
}

/** Set the user on the active scope (the global scope outside a `withScope`). */
export function setUser(user: User | null): void {
  getCurrentScope().setUser(user);
}

/** Set a single tag on the active scope. */
export function setTag(key: string, value: string): void {
  getCurrentScope().setTag(key, value);
}

/** Merge several tags onto the active scope. */
export function setTags(tags: Record<string, string>): void {
  getCurrentScope().setTags(tags);
}

/** Set a named free-form context block on the active scope. */
export function setContext(key: string, context: Record<string, unknown> | unknown): void {
  getCurrentScope().setContext(key, context);
}

/** Set a single free-form extra value on the active scope. */
export function setExtra(key: string, value: unknown): void {
  getCurrentScope().setExtra(key, value);
}

/** Flush buffered items immediately. Resolves once sent (or if not init). */
export function flush(): Promise<void> {
  return activeClient ? activeClient.flush() : Promise.resolve();
}

/** Flush and stop the background timer, then clear the active client. */
export async function close(): Promise<void> {
  if (!activeClient) return;
  const client = activeClient;
  activeClient = null;
  await client.close();
}
