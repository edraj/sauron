import type { Dsn } from '../dsn.js';
import { beginInternal, endInternal } from '../integrations/instrument.js';
import type { Envelope, EnvelopeItem } from '../types.js';
import { byteLength, computeBackoff, safeStringify } from '../utils.js';
import { maybeCompress } from './compress.js';
import { defaultStorage, OfflineQueue } from './queue.js';

/** `sendBeacon` / `keepalive` payloads are capped near 64 KiB by browsers. */
export const BEACON_MAX_BYTES = 64 * 1024;

/** Max delivery attempts before a batch is parked in the offline queue. */
const MAX_RETRIES = 5;

/** Retry-After is honored but clamped so `flush()`/`close()` can't hang forever. */
const RETRY_AFTER_CAP_MS = 30_000;

type SendAction = 'drop' | 'disable' | 'split' | 'retry_after' | 'retry_backoff';

interface SendOutcome {
  action: SendAction;
  retryAfterMs?: number;
}

interface HttpResult {
  status: number;
  retryAfter: string | null;
}

interface Logger {
  log: (...args: unknown[]) => void;
  warn: (...args: unknown[]) => void;
}

export interface TransportConfig {
  dsn: Dsn;
  options: { flushIntervalMs: number; maxBatch: number; maxQueueBytes: number };
  makeEnvelope: (items: EnvelopeItem[]) => Envelope;
  fetchImpl?: typeof fetch;
  logger: Logger;
  onDisable: () => void;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, Math.max(0, ms)));
}

function parseRetryAfter(header: string | null): number {
  if (!header) return 1000;
  const seconds = Number(header);
  if (!Number.isNaN(seconds)) {
    return Math.min(RETRY_AFTER_CAP_MS, Math.max(0, seconds * 1000));
  }
  const date = Date.parse(header);
  if (!Number.isNaN(date)) {
    return Math.min(RETRY_AFTER_CAP_MS, Math.max(0, date - Date.now()));
  }
  return 1000;
}

/** Map an HTTP status onto the locked response-handling table. */
function classifyStatus(status: number, retryAfter: string | null): SendOutcome {
  if (status === 200 || status === 202) return { action: 'drop' };
  if (status === 400) return { action: 'drop' };
  if (status === 401 || status === 403) return { action: 'disable' };
  if (status === 408) return { action: 'retry_backoff' };
  if (status === 413) return { action: 'split' };
  if (status === 429) return { action: 'retry_after', retryAfterMs: parseRetryAfter(retryAfter) };
  if (status >= 500) return { action: 'retry_backoff' };
  if (status >= 400) return { action: 'drop' }; // other 4xx: client error, no retry
  return { action: 'drop' };
}

/**
 * Batching transport with retry/backoff, an offline queue and a beacon path.
 *
 * All outbound requests go through `fetchImpl` (the NATIVE fetch captured before
 * integrations wrapped it) or an XHR fallback, so the transport never triggers
 * its own instrumentation.
 */
export class Transport {
  private readonly dsn: Dsn;
  private readonly makeEnvelope: (items: EnvelopeItem[]) => Envelope;
  private readonly fetchImpl?: typeof fetch;
  private readonly logger: Logger;
  private readonly onDisable: () => void;
  private readonly flushIntervalMs: number;
  private readonly maxBatch: number;
  private readonly offline: OfflineQueue;

  private pending: EnvelopeItem[] = [];
  private timer: ReturnType<typeof setInterval> | null = null;
  private onlineHandler: (() => void) | null = null;
  private disabled = false;

  constructor(config: TransportConfig) {
    this.dsn = config.dsn;
    this.makeEnvelope = config.makeEnvelope;
    this.fetchImpl = config.fetchImpl;
    this.logger = config.logger;
    this.onDisable = config.onDisable;
    this.flushIntervalMs = config.options.flushIntervalMs;
    this.maxBatch = Math.max(1, config.options.maxBatch);
    this.offline = new OfflineQueue(config.options.maxQueueBytes, defaultStorage());
  }

  /** Begin periodic flushing and listen for connectivity restoration. */
  start(): void {
    if (this.timer === null && this.flushIntervalMs > 0) {
      this.timer = setInterval(() => {
        void this.flush();
      }, this.flushIntervalMs);
      // Don't hold the Node event loop open (harmless/no-op in browsers).
      (this.timer as { unref?: () => void }).unref?.();
    }
    const g = globalThis as { addEventListener?: (t: string, h: () => void) => void };
    if (typeof g.addEventListener === 'function' && !this.onlineHandler) {
      this.onlineHandler = () => {
        void this.drainOfflineQueue();
      };
      g.addEventListener('online', this.onlineHandler);
    }
  }

  /** Stop timers and listeners (does not flush). */
  stop(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
    const g = globalThis as { removeEventListener?: (t: string, h: () => void) => void };
    if (this.onlineHandler && typeof g.removeEventListener === 'function') {
      g.removeEventListener('online', this.onlineHandler);
      this.onlineHandler = null;
    }
  }

  /** Permanently disable the transport (401/403). Drops pending work. */
  disable(): void {
    this.disabled = true;
    this.pending = [];
    this.stop();
  }

  /** Queue an item for the next batch; flush eagerly once the batch is full. */
  send(item: EnvelopeItem): void {
    if (this.disabled) return;
    this.pending.push(item);
    if (this.pending.length >= this.maxBatch) {
      void this.flush();
    }
  }

  /**
   * Flush all pending items (plus a drain of the offline queue). Resolves to
   * `true` on completion, or `false` if `timeoutMs` elapsed first.
   */
  async flush(timeoutMs?: number): Promise<boolean> {
    if (this.disabled) return true;
    const items = this.pending.splice(0, this.pending.length);

    const work = (async (): Promise<boolean> => {
      await this.drainOfflineQueue();
      for (let i = 0; i < items.length; i += this.maxBatch) {
        await this.deliver(items.slice(i, i + this.maxBatch));
      }
      return true;
    })();

    if (typeof timeoutMs === 'number' && timeoutMs >= 0) {
      return Promise.race([work, delay(timeoutMs).then(() => false)]);
    }
    return work;
  }

  /** Send an already-serialized batch, applying the retry/backoff policy. */
  private async deliver(items: EnvelopeItem[], attempt = 0): Promise<void> {
    if (this.disabled || items.length === 0) return;

    const json = safeStringify(this.makeEnvelope(items));
    let outcome: SendOutcome;
    try {
      outcome = await this.post(json);
    } catch {
      outcome = { action: 'retry_backoff' }; // network error
    }

    switch (outcome.action) {
      case 'drop':
        return;
      case 'disable':
        this.logger.warn('server rejected credentials; disabling client');
        this.onDisable();
        return;
      case 'split': {
        if (items.length <= 1) {
          this.logger.warn('single item too large (413); parking offline');
          this.offline.enqueue(json);
          return;
        }
        const mid = Math.ceil(items.length / 2);
        await this.deliver(items.slice(0, mid), attempt);
        await this.deliver(items.slice(mid), attempt);
        return;
      }
      case 'retry_after':
      case 'retry_backoff': {
        if (attempt >= MAX_RETRIES) {
          this.offline.enqueue(json);
          return;
        }
        const wait =
          outcome.action === 'retry_after'
            ? (outcome.retryAfterMs ?? 1000)
            : computeBackoff(attempt);
        await delay(wait);
        return this.deliver(items, attempt + 1);
      }
    }
  }

  /** Re-attempt any envelopes that were parked while offline. */
  async drainOfflineQueue(): Promise<void> {
    if (this.disabled || !this.offline.available) return;
    const payloads = this.offline.drain();
    for (const json of payloads) {
      let outcome: SendOutcome;
      try {
        outcome = await this.post(json);
      } catch {
        outcome = { action: 'retry_backoff' };
      }
      if (outcome.action === 'disable') {
        this.onDisable();
        this.offline.enqueue(json);
        return;
      }
      if (outcome.action === 'retry_after' || outcome.action === 'retry_backoff') {
        // Still failing — re-park and stop draining to avoid a tight loop.
        this.offline.enqueue(json);
        return;
      }
      // drop / split: treat as handled.
    }
  }

  /** Best-effort synchronous-ish flush for page unload via `sendBeacon`. */
  flushToBeacon(): void {
    if (this.disabled) return;
    const items = this.pending.splice(0, this.pending.length);
    if (items.length === 0) return;

    const json = safeStringify(this.makeEnvelope(items));
    const nav = (globalThis as { navigator?: Navigator }).navigator;
    const size = byteLength(json);

    if (nav && typeof nav.sendBeacon === 'function' && size <= BEACON_MAX_BYTES) {
      try {
        const blob = new Blob([json], { type: 'application/json' });
        if (nav.sendBeacon(this.dsn.beaconUrl, blob)) return;
      } catch {
        /* fall through to offline queue */
      }
    }
    // Beacon unavailable or oversized: persist for the next page load.
    this.offline.enqueue(json);
  }

  /** Compress (when large) and POST one serialized envelope. */
  private async post(json: string): Promise<SendOutcome> {
    const { body, encoding } = await maybeCompress(json);
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      'X-Sauron-Key': this.dsn.publicKey,
    };
    if (encoding) headers['Content-Encoding'] = encoding;

    const size = typeof body === 'string' ? byteLength(body) : body.byteLength;
    const result = await this.doRequest(headers, body, size <= BEACON_MAX_BYTES);
    return classifyStatus(result.status, result.retryAfter);
  }

  private async doRequest(
    headers: Record<string, string>,
    body: Uint8Array | string,
    keepalive: boolean,
  ): Promise<HttpResult> {
    if (this.fetchImpl) {
      beginInternal();
      let promise: Promise<Response>;
      try {
        promise = this.fetchImpl(this.dsn.envelopeUrl, {
          method: 'POST',
          headers,
          body: body as BodyInit,
          keepalive,
        });
      } finally {
        endInternal();
      }
      const res = await promise;
      return { status: res.status, retryAfter: res.headers?.get?.('Retry-After') ?? null };
    }
    return this.xhrRequest(headers, body);
  }

  private xhrRequest(
    headers: Record<string, string>,
    body: Uint8Array | string,
  ): Promise<HttpResult> {
    return new Promise<HttpResult>((resolve, reject) => {
      const XHR = (globalThis as { XMLHttpRequest?: typeof XMLHttpRequest }).XMLHttpRequest;
      if (typeof XHR !== 'function') {
        reject(new Error('no transport available'));
        return;
      }
      beginInternal();
      try {
        const xhr = new XHR();
        xhr.open('POST', this.dsn.envelopeUrl, true);
        for (const key of Object.keys(headers)) {
          try {
            xhr.setRequestHeader(key, headers[key]);
          } catch {
            /* forbidden header — skip */
          }
        }
        xhr.onload = () =>
          resolve({ status: xhr.status, retryAfter: safeHeader(xhr, 'Retry-After') });
        xhr.onerror = () => reject(new Error('xhr network error'));
        xhr.ontimeout = () => reject(new Error('xhr timeout'));
        xhr.send(body as XMLHttpRequestBodyInit);
      } catch (err) {
        reject(err instanceof Error ? err : new Error(String(err)));
      } finally {
        endInternal();
      }
    });
  }

  /** Offline queue accessor (used by tests / diagnostics). */
  get offlineQueue(): OfflineQueue {
    return this.offline;
  }
}

function safeHeader(xhr: XMLHttpRequest, name: string): string | null {
  try {
    return xhr.getResponseHeader(name);
  } catch {
    return null;
  }
}
