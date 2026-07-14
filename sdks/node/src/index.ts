/**
 * @sauron/node — server-side Node/TypeScript SDK.
 *
 * Dispatches product-analytics events and captured exceptions to the Sauron
 * ingest gateway. No browser/DOM/auto-instrumentation — a buffered background
 * HTTP transport over Node's global `fetch`.
 */

import { SauronClient } from './client.js';
import type { CaptureExceptionOptions, InitOptions, Level } from './types.js';

export { SauronClient, describeError } from './client.js';
export { parseDsn, DsnError } from './dsn.js';
export type { Dsn } from './dsn.js';
export { parseStackString, parseError, isInAppFrame } from './stacktrace.js';
export { Transport } from './transport.js';
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
): void {
  activeClient?.track(event, distinctId, properties);
}

/** Capture a native `Error`. No-op if the SDK is not initialized. */
export function captureException(error: unknown, options?: CaptureExceptionOptions): void {
  activeClient?.captureException(error, options);
}

/** Capture a bare message. No-op if the SDK is not initialized. */
export function captureMessage(message: string, level?: Level): void {
  activeClient?.captureMessage(message, level);
}

/** Associate traits with a distinct id. No-op if the SDK is not initialized. */
export function identify(distinctId: string, traits?: Record<string, unknown>): void {
  activeClient?.identify(distinctId, traits);
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
