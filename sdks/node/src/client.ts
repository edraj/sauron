import os from 'node:os';
import { randomUUID } from 'node:crypto';

import { parseDsn } from './dsn.js';
import { Transport } from './transport.js';
import { parseError } from './stacktrace.js';
import { installAutoCapture, installShutdownHooks } from './autocapture.js';
import {
  getCurrentScope,
  getGlobalScope,
  normalizeBreadcrumb,
} from './scope.js';
import type {
  BreadcrumbInput,
  CaptureExceptionOptions,
  Context,
  EnvelopeItem,
  ErrorItem,
  ErrorUser,
  EventItem,
  IdentifyItem,
  InitOptions,
  Level,
  MetadataOptions,
  ResolvedOptions,
  TransactionInput,
  TransactionItem,
} from './types.js';

const DEFAULTS = {
  environment: 'production',
  release: null as string | null,
  sampleRate: 1,
  flushInterval: 5000,
  maxBatch: 30,
  maxBreadcrumbs: 100,
  gzipThresholdBytes: 1024,
  maxQueueBytes: 1_048_576,
  maxRetries: 3,
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
    tags: options.tags ?? {},
    contexts: options.contexts ?? {},
    extra: options.extra ?? {},
    sampleRate: Math.min(1, Math.max(0, sampleRate)),
    flushInterval:
      typeof options.flushInterval === 'number'
        ? options.flushInterval
        : DEFAULTS.flushInterval,
    maxBatch: typeof options.maxBatch === 'number' ? options.maxBatch : DEFAULTS.maxBatch,
    maxBreadcrumbs:
      typeof options.maxBreadcrumbs === 'number'
        ? options.maxBreadcrumbs
        : DEFAULTS.maxBreadcrumbs,
    gzipThresholdBytes:
      typeof options.gzipThresholdBytes === 'number'
        ? options.gzipThresholdBytes
        : DEFAULTS.gzipThresholdBytes,
    maxQueueBytes:
      typeof options.maxQueueBytes === 'number'
        ? options.maxQueueBytes
        : DEFAULTS.maxQueueBytes,
    offlineDir: options.offlineDir ?? null,
    maxRetries:
      typeof options.maxRetries === 'number' ? options.maxRetries : DEFAULTS.maxRetries,
    autoCaptureUnhandled: options.autoCaptureUnhandled ?? false,
    autoShutdown: options.autoShutdown ?? false,
    beforeSend: options.beforeSend,
    beforeBreadcrumb: options.beforeBreadcrumb,
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
  /** Uninstallers for any opt-in process-level hooks, torn down on {@link close}. */
  private readonly hookUninstallers: Array<() => void> = [];

  constructor(options: InitOptions) {
    this.options = resolveOptions(options);
    const dsn = parseDsn(this.options.dsn);
    const globalScope = getGlobalScope();
    globalScope.setMaxBreadcrumbs(this.options.maxBreadcrumbs);
    globalScope.setTags(this.options.tags);
    for (const [name, block] of Object.entries(this.options.contexts)) {
      globalScope.setContext(name, block);
    }
    for (const [key, value] of Object.entries(this.options.extra)) {
      globalScope.setExtra(key, value);
    }
    this.transport = new Transport({
      dsn,
      environment: this.options.environment,
      release: this.options.release,
      context: buildContext(),
      flushInterval: this.options.flushInterval,
      maxBatch: this.options.maxBatch,
      gzipThresholdBytes: this.options.gzipThresholdBytes,
      maxQueueBytes: this.options.maxQueueBytes,
      offlineDir: this.options.offlineDir,
      maxRetries: this.options.maxRetries,
      fetchImpl: this.options.fetchImpl,
      debug: this.options.debug,
    });
    if (this.options.autoCaptureUnhandled) {
      this.hookUninstallers.push(installAutoCapture(this));
    }
    if (this.options.autoShutdown) {
      this.hookUninstallers.push(installShutdownHooks(this));
    }
  }

  /**
   * The single enqueue chokepoint. Runs `beforeSend` on every item; a `null`
   * return drops it, a returned item replaces it, then it is handed to the
   * transport.
   */
  private dispatch(item: EnvelopeItem): void {
    const beforeSend = this.options.beforeSend;
    if (beforeSend) {
      const result = beforeSend(item);
      if (result == null) return;
      this.transport.enqueue(result);
      return;
    }
    this.transport.enqueue(item);
  }

  /**
   * Add a breadcrumb to the active scope. Runs `beforeBreadcrumb` first; a
   * `null` return drops the crumb.
   */
  addBreadcrumb(crumb: BreadcrumbInput): void {
    const stamped = normalizeBreadcrumb(crumb);
    const beforeBreadcrumb = this.options.beforeBreadcrumb;
    if (beforeBreadcrumb) {
      const result = beforeBreadcrumb(stamped);
      if (result == null) return;
      getCurrentScope().addBreadcrumb(result);
      return;
    }
    getCurrentScope().addBreadcrumb(stamped);
  }

  /** Emit a performance transaction item. */
  trackTransaction(input: TransactionInput): void {
    if (typeof input?.name !== 'string' || input.name.length === 0) return;
    const distinctId = input.distinct_id ?? getCurrentScope().data.user?.id ?? undefined;
    const item: TransactionItem = {
      type: 'transaction',
      name: input.name,
      op: input.op ?? 'custom',
      duration_ms: input.duration_ms,
      timestamp: isoNow(),
    };
    if (input.status !== undefined) item.status = input.status;
    if (input.http_method !== undefined) item.http_method = input.http_method;
    if (input.http_status !== undefined) item.http_status = input.http_status;
    if (input.url !== undefined) item.url = input.url;
    if (distinctId != null) item.distinct_id = distinctId;
    this.dispatch(item);
  }

  /** Capture a product-analytics event. `distinctId` is required. */
  track(
    event: string,
    distinctId: string,
    properties?: Record<string, unknown>,
    options: MetadataOptions = {},
  ): void {
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
      ...getCurrentScope().mergeMetadata(options),
    };
    this.dispatch(item);
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
      contexts: options.contexts ?? {},
      extra: options.extra ?? {},
      fingerprint: options.fingerprint ?? null,
      user: normalizeUser(options.user),
      session_id: null,
      screen: null,
    };
    getCurrentScope().applyToErrorItem(item);
    this.dispatch(item);
  }

  /** Capture a bare message as an error item (no exception payload). */
  captureMessage(message: string, level: Level = 'info', options: MetadataOptions = {}): void {
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
      tags: options.tags ?? {},
      contexts: options.contexts ?? {},
      extra: options.extra ?? {},
      fingerprint: null,
      user: null,
      session_id: null,
      screen: null,
    };
    getCurrentScope().applyToErrorItem(item);
    this.dispatch(item);
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
    this.dispatch(item);
  }

  /** Send any buffered items immediately. */
  flush(): Promise<void> {
    return this.transport.flush();
  }

  /** Flush then stop the background timer, and remove any opt-in process hooks. */
  close(): Promise<void> {
    for (const uninstall of this.hookUninstallers.splice(0)) uninstall();
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
