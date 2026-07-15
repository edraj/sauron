import type { Dsn } from './dsn.js';
import { maybeGzip } from './gzip.js';
import { BoundedQueue } from './queue.js';
import type {
  Context,
  Envelope,
  EnvelopeHeader,
  EnvelopeItem,
  FetchLike,
  SleepFn,
} from './types.js';

const SDK_NAME = 'sauron-node';
const SDK_VERSION = '0.3.0';

/** Default exponential-backoff base (ms) for the first retry. */
const DEFAULT_RETRY_BASE_MS = 200;
/** Hard cap on any single backoff delay (ms). */
const RETRY_CAP_MS = 30_000;

export interface TransportConfig {
  dsn: Dsn;
  environment: string;
  release: string | null;
  context: Context;
  flushInterval: number;
  maxBatch: number;
  fetchImpl?: FetchLike;
  debug: boolean;
  /** Gzip the body once it exceeds this many bytes. Default 1024. */
  gzipThresholdBytes?: number;
  /** Drop-oldest byte cap for the in-memory buffer. Default 1 MiB. */
  maxQueueBytes?: number;
  /** Opt-in directory for FIFO disk persistence. Default off. */
  offlineDir?: string | null;
  /** Max retries after the first attempt. Default 3. */
  maxRetries?: number;
  /** Backoff base (ms). Default 200. */
  retryBaseMs?: number;
  /** Deterministic sleep seam (tests). Defaults to a real timer. */
  sleep?: SleepFn;
  /** Jitter source (tests). Defaults to `Math.random`. */
  random?: () => number;
}

/** Statuses that are worth retrying (plus any 5xx and network errors). */
function isRetryableStatus(status: number): boolean {
  return status === 408 || status === 413 || status === 429 || status >= 500;
}

/**
 * Parse a `Retry-After` header (delta-seconds or an HTTP-date) into a delay in
 * ms, clamped to non-negative. Returns `null` when absent/unparseable.
 */
export function parseRetryAfter(value: string | null | undefined, nowMs: number): number | null {
  if (value == null) return null;
  const trimmed = value.trim();
  if (trimmed === '') return null;
  const secs = Number(trimmed);
  if (Number.isFinite(secs)) return Math.max(0, secs * 1000);
  const dateMs = Date.parse(trimmed);
  if (!Number.isNaN(dateMs)) return Math.max(0, dateMs - nowMs);
  return null;
}

const realSleep: SleepFn = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Buffered background transport.
 *
 * Items accumulate in a byte-bounded {@link BoundedQueue} (optionally persisted
 * to disk). A `setInterval` timer (unref'd, so it never keeps the event loop
 * alive) flushes every `flushInterval` ms, and an eager flush fires once the
 * queue reaches `maxBatch`. Each flush drains a batch into one envelope, gzips
 * it when large, and POSTs it with an exponential-backoff retry policy:
 *
 * - retry 408/413/429/5xx and network errors (honoring `Retry-After` on 429),
 * - drop (no retry) on 400/401/403/404 — 401/403 also disable the SDK,
 * - after `maxRetries` transient failures, the batch is re-buffered (kept for a
 *   later flush / next process start) rather than lost.
 *
 * Flushes are serialized through a promise chain so a batch is never drained by
 * two overlapping flushes.
 */
export class Transport {
  private readonly buffer: BoundedQueue;
  private timer: ReturnType<typeof setInterval> | null = null;
  private readonly fetchImpl: FetchLike;
  private disabled = false;
  private readonly gzipThreshold: number;
  private readonly maxRetries: number;
  private readonly retryBaseMs: number;
  private readonly sleep: SleepFn;
  private readonly random: () => number;
  /** Serializes flushes so a batch is drained/committed atomically. */
  private flushChain: Promise<void> = Promise.resolve();

  constructor(private readonly config: TransportConfig) {
    const injected = config.fetchImpl;
    const globalFetch = (globalThis as { fetch?: unknown }).fetch as
      | FetchLike
      | undefined;
    const chosen = injected ?? globalFetch;
    if (!chosen) {
      throw new Error(
        '[sauron] global fetch is unavailable (Node >= 18 required) and no fetchImpl was provided',
      );
    }
    this.fetchImpl = chosen;
    this.gzipThreshold = config.gzipThresholdBytes ?? 1024;
    this.maxRetries = Math.max(0, config.maxRetries ?? 3);
    this.retryBaseMs = config.retryBaseMs ?? DEFAULT_RETRY_BASE_MS;
    this.sleep = config.sleep ?? realSleep;
    this.random = config.random ?? Math.random;
    this.buffer = new BoundedQueue({
      maxBytes: config.maxQueueBytes ?? 1_048_576,
      offlineDir: config.offlineDir ?? null,
    });
    this.startTimer();
  }

  private startTimer(): void {
    if (this.timer || this.config.flushInterval <= 0) return;
    this.timer = setInterval(() => {
      void this.flush();
    }, this.config.flushInterval);
    // Do not hold the event loop open for the flush timer.
    if (typeof this.timer.unref === 'function') this.timer.unref();
  }

  /** Enqueue an item; triggers an eager flush at `maxBatch`. */
  enqueue(item: EnvelopeItem): void {
    if (this.disabled) return;
    this.buffer.push(item);
    if (this.buffer.size >= this.config.maxBatch) {
      void this.flush();
    }
  }

  private buildEnvelope(items: EnvelopeItem[]): Envelope {
    const header: EnvelopeHeader = {
      dsn: this.config.dsn.raw,
      sdk: { name: SDK_NAME, version: SDK_VERSION },
      sent_at: new Date().toISOString(),
      environment: this.config.environment,
      release: this.config.release,
    };
    return { header, context: this.config.context, items };
  }

  /**
   * Drain a batch and send it. Serialized through {@link flushChain} so
   * overlapping calls (eager + timer + manual) never race the queue.
   */
  flush(): Promise<void> {
    const next = this.flushChain.then(() => this.drainAndSend());
    // Swallow rejections on the chain so one failure can't poison later flushes.
    this.flushChain = next.catch(() => undefined);
    return this.flushChain;
  }

  private async drainAndSend(): Promise<void> {
    if (this.disabled) return;
    const items = this.buffer.drain();
    if (items.length === 0) {
      this.buffer.commit();
      return;
    }
    await this.send(this.buildEnvelope(items));
  }

  private async send(envelope: Envelope): Promise<void> {
    const { dsn } = this.config;
    const raw = JSON.stringify(envelope);
    const { body, headers: encodingHeaders } = maybeGzip(raw, this.gzipThreshold);
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      'X-Sauron-Key': dsn.publicKey,
      ...encodingHeaders,
    };

    let attempt = 0;
    for (;;) {
      let retryAfterMs: number | null = null;
      try {
        const res = await this.fetchImpl(dsn.envelopeUrl, {
          method: 'POST',
          headers,
          body,
        });
        const status = res.status;
        if (status >= 200 && status < 300) {
          this.buffer.commit();
          return;
        }
        if (status === 401 || status === 403) {
          this.disabled = true;
          this.log(`auth failed (${status}); disabling SDK`);
          this.buffer.commit();
          return;
        }
        if (!isRetryableStatus(status)) {
          this.log(`ingest returned ${status}; dropping ${envelope.items.length} item(s)`);
          this.buffer.commit();
          return;
        }
        // Retryable HTTP status.
        if (status === 429) {
          retryAfterMs = parseRetryAfter(res.headers?.get('retry-after'), Date.now());
        }
        this.log(`ingest returned ${status}; will retry (attempt ${attempt + 1})`);
      } catch (err) {
        // Network-level failure — retryable.
        this.log(`transport error: ${String(err)} (attempt ${attempt + 1})`);
      }

      if (attempt >= this.maxRetries) {
        // Out of retries: keep the batch buffered for a later flush / restart
        // rather than dropping it on the floor.
        this.buffer.restore();
        this.log(`giving up after ${attempt + 1} attempt(s); re-buffering batch`);
        return;
      }
      const delay = retryAfterMs ?? this.backoffDelay(attempt);
      await this.sleep(Math.min(delay, RETRY_CAP_MS));
      attempt += 1;
    }
  }

  /** Exponential backoff with equal-jitter, capped at {@link RETRY_CAP_MS}. */
  private backoffDelay(attempt: number): number {
    const exp = Math.min(RETRY_CAP_MS, this.retryBaseMs * 2 ** attempt);
    const half = exp / 2;
    return Math.min(RETRY_CAP_MS, half + this.random() * half);
  }

  /** Flush then stop the timer. After close the transport still accepts a
   * final manual `flush`, but the background timer no longer fires. */
  async close(): Promise<void> {
    await this.flush();
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    await this.flushChain;
  }

  private log(message: string): void {
    if (this.config.debug) {
      // eslint-disable-next-line no-console
      console.warn(`[sauron] ${message}`);
    }
  }
}
