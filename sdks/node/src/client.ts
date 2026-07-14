import os from 'node:os';
import { randomUUID } from 'node:crypto';

import { parseDsn } from './dsn.js';
import { Transport } from './transport.js';
import { parseError } from './stacktrace.js';
import type {
  CaptureExceptionOptions,
  Context,
  ErrorItem,
  ErrorUser,
  EventItem,
  IdentifyItem,
  InitOptions,
  Level,
  ResolvedOptions,
} from './types.js';

const DEFAULTS = {
  environment: 'production',
  release: null as string | null,
  sampleRate: 1,
  flushInterval: 5000,
  maxBatch: 30,
  debug: false,
};

function resolveOptions(options: InitOptions): ResolvedOptions {
  if (!options || typeof options.dsn !== 'string') {
    throw new Error('[sauron] init requires a { dsn } option');
  }
  const sampleRate =
    typeof options.sampleRate === 'number' ? options.sampleRate : DEFAULTS.sampleRate;
  return {
    dsn: options.dsn,
    environment: options.environment ?? DEFAULTS.environment,
    release: options.release ?? DEFAULTS.release,
    sampleRate: Math.min(1, Math.max(0, sampleRate)),
    flushInterval:
      typeof options.flushInterval === 'number'
        ? options.flushInterval
        : DEFAULTS.flushInterval,
    maxBatch: typeof options.maxBatch === 'number' ? options.maxBatch : DEFAULTS.maxBatch,
    fetchImpl: options.fetchImpl,
    debug: options.debug ?? DEFAULTS.debug,
  };
}

/** Minimal server-side context assembled once at init. */
function buildContext(): Context {
  return {
    device: { device_id: randomUUID() },
    os: { name: process.platform || null, version: os.release() || null },
    app: {},
    runtime: { name: 'node', version: process.versions.node ?? null },
    user: null,
  };
}

function isoNow(): string {
  return new Date().toISOString();
}

function normalizeUser(user: Partial<ErrorUser> | null | undefined): ErrorUser | null {
  if (!user) return null;
  return {
    id: user.id ?? null,
    email: user.email ?? null,
    username: user.username ?? null,
  };
}

/**
 * The Sauron server-side client. Buffers events/errors and dispatches them via
 * a background transport. Constructed by {@link init}.
 */
export class SauronClient {
  private readonly options: ResolvedOptions;
  private readonly transport: Transport;

  constructor(options: InitOptions) {
    this.options = resolveOptions(options);
    const dsn = parseDsn(this.options.dsn);
    this.transport = new Transport({
      dsn,
      environment: this.options.environment,
      release: this.options.release,
      context: buildContext(),
      flushInterval: this.options.flushInterval,
      maxBatch: this.options.maxBatch,
      fetchImpl: this.options.fetchImpl,
      debug: this.options.debug,
    });
  }

  /** Capture a product-analytics event. `distinctId` is required. */
  track(event: string, distinctId: string, properties?: Record<string, unknown>): void {
    if (typeof event !== 'string' || event.length === 0) return;
    if (typeof distinctId !== 'string' || distinctId.length === 0) return;
    const item: EventItem = {
      type: 'event',
      name: event,
      distinct_id: distinctId,
      properties: properties ?? {},
      timestamp: isoNow(),
      session_id: null,
      screen: null,
    };
    this.transport.enqueue(item);
  }

  /** Capture a native `Error` (or error-like value) as an error item. */
  captureException(error: unknown, options: CaptureExceptionOptions = {}): void {
    if (this.options.sampleRate < 1 && Math.random() >= this.options.sampleRate) {
      return;
    }
    const { type, value } = describeError(error);
    const item: ErrorItem = {
      type: 'error',
      event_id: randomUUID(),
      level: options.level ?? 'error',
      timestamp: isoNow(),
      exception: {
        type,
        value,
        mechanism: { type: 'generic', handled: options.handled ?? true },
        stacktrace: parseError(error),
      },
      message: null,
      breadcrumbs: [],
      tags: options.tags ?? {},
      fingerprint: null,
      user: normalizeUser(options.user),
      session_id: null,
      screen: null,
    };
    this.transport.enqueue(item);
  }

  /** Capture a bare message as an error item (no exception payload). */
  captureMessage(message: string, level: Level = 'info'): void {
    const item: ErrorItem = {
      type: 'error',
      event_id: randomUUID(),
      level,
      timestamp: isoNow(),
      exception: {
        type: 'Message',
        value: message,
        mechanism: { type: 'generic', handled: true },
        stacktrace: [],
      },
      message,
      breadcrumbs: [],
      tags: {},
      fingerprint: null,
      user: null,
      session_id: null,
      screen: null,
    };
    this.transport.enqueue(item);
  }

  /** Associate traits with a distinct id. */
  identify(distinctId: string, traits?: Record<string, unknown>): void {
    if (typeof distinctId !== 'string' || distinctId.length === 0) return;
    const item: IdentifyItem = {
      type: 'identify',
      distinct_id: distinctId,
      anonymous_id: null,
      traits: traits ?? {},
      timestamp: isoNow(),
    };
    this.transport.enqueue(item);
  }

  /** Send any buffered items immediately. */
  flush(): Promise<void> {
    return this.transport.flush();
  }

  /** Flush then stop the background timer. */
  close(): Promise<void> {
    return this.transport.close();
  }
}

/** Derive `{type, value}` from an arbitrary thrown value. */
export function describeError(error: unknown): { type: string; value: string | null } {
  if (error instanceof Error) {
    return { type: error.name || 'Error', value: error.message || null };
  }
  if (typeof error === 'string') {
    return { type: 'Error', value: error };
  }
  if (error && typeof error === 'object') {
    const name = (error as { name?: unknown }).name;
    const message = (error as { message?: unknown }).message;
    return {
      type: typeof name === 'string' && name ? name : 'Error',
      value: typeof message === 'string' ? message : null,
    };
  }
  return { type: 'Error', value: error === undefined ? null : String(error) };
}
