import type { Dsn } from './dsn.js';
import type {
  Context,
  Envelope,
  EnvelopeHeader,
  EnvelopeItem,
  FetchLike,
} from './types.js';

const SDK_NAME = 'sauron-node';
const SDK_VERSION = '0.1.0';

export interface TransportConfig {
  dsn: Dsn;
  environment: string;
  release: string | null;
  context: Context;
  flushInterval: number;
  maxBatch: number;
  fetchImpl?: FetchLike;
  debug: boolean;
}

/**
 * Buffered background transport.
 *
 * Items accumulate in an in-memory queue; a `setInterval` timer (unref'd, so it
 * never keeps the event loop alive) flushes every `flushInterval` ms, and an
 * eager flush fires once the queue reaches `maxBatch`. Each flush drains the
 * queue into a single envelope and POSTs it.
 *
 * On a hard auth failure (401/403) the transport disables itself and stops
 * sending. Transient failures (network error, 429/5xx) drop the current batch
 * after logging (v0.1: no offline queue, bounded/no retry).
 */
export class Transport {
  private queue: EnvelopeItem[] = [];
  private timer: ReturnType<typeof setInterval> | null = null;
  private readonly fetchImpl: FetchLike;
  private disabled = false;
  /** Tracks in-flight sends so `flush()`/`close()` can await them. */
  private pending: Set<Promise<void>> = new Set();

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
    this.queue.push(item);
    if (this.queue.length >= this.config.maxBatch) {
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

  /** Drain the queue and send it immediately; awaits the network round-trip. */
  async flush(): Promise<void> {
    if (this.disabled) return;
    if (this.queue.length === 0) {
      // Still await any already in-flight sends.
      await Promise.all([...this.pending]);
      return;
    }
    const items = this.queue;
    this.queue = [];
    const envelope = this.buildEnvelope(items);
    const send = this.send(envelope);
    this.pending.add(send);
    try {
      await send;
    } finally {
      this.pending.delete(send);
    }
  }

  private async send(envelope: Envelope): Promise<void> {
    const { dsn } = this.config;
    try {
      const res = await this.fetchImpl(dsn.envelopeUrl, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-Sauron-Key': dsn.publicKey,
        },
        body: JSON.stringify(envelope),
      });
      const status = res.status;
      if (status === 401 || status === 403) {
        this.disabled = true;
        this.log(`auth failed (${status}); disabling SDK`);
        return;
      }
      if (status >= 400) {
        this.log(`ingest returned ${status}; dropping ${envelope.items.length} item(s)`);
      }
    } catch (err) {
      this.log(`transport error: ${String(err)}`);
    }
  }

  /** Flush then stop the timer. After close the transport still accepts a
   * final manual `flush`, but the background timer no longer fires. */
  async close(): Promise<void> {
    await this.flush();
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    await Promise.all([...this.pending]);
  }

  private log(message: string): void {
    if (this.config.debug) {
      // eslint-disable-next-line no-console
      console.warn(`[sauron] ${message}`);
    }
  }
}
