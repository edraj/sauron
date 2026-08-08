import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { parseDsn } from '../src/dsn.js';
import { Transport } from '../src/transport/transport.js';
import type { StorageLike } from '../src/transport/queue.js';
import type { Envelope, EnvelopeItem } from '../src/types.js';

/**
 * `Transport.drainOfflineQueue()` — the loop `queue.test.ts` never drove.
 *
 * `OfflineQueue.drain()` empties `localStorage` in one shot and hands the caller
 * the only remaining copy. The drain loop used to re-park just the payload that
 * failed and return, so every payload BEHIND it was already deleted from storage
 * and vanished with the local array. That is precisely the flaky-network case
 * the queue exists for: reconnect, the first envelope 500s, the rest of the
 * backlog evaporates — no log, no retry.
 */

/** In-memory `localStorage` stand-in. */
class MemoryStorage implements StorageLike {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? (this.map.get(key) as string) : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
}

const DSN = 'https://pk_test@localhost:8081/1';

interface Harness {
  transport: Transport;
  /** Statuses handed back in order; the last one repeats once exhausted. */
  statuses: number[];
  posted: string[];
  disabled: boolean;
}

const g = globalThis as { localStorage?: StorageLike };
const originalLocalStorage = g.localStorage;

function makeHarness(statuses: number[]): Harness {
  const harness: Harness = {
    statuses: [...statuses],
    posted: [],
    disabled: false,
    transport: undefined as unknown as Transport,
  };

  const fetchImpl = (async (_url: unknown, init: { body: string }) => {
    harness.posted.push(init.body);
    const status = harness.statuses.length > 1 ? (harness.statuses.shift() as number) : harness.statuses[0];
    return new Response(null, { status });
  }) as unknown as typeof fetch;

  harness.transport = new Transport({
    dsn: parseDsn(DSN),
    // `flushIntervalMs: 0` keeps the periodic timer off; nothing here calls start().
    options: { flushIntervalMs: 0, maxBatch: 1000, maxQueueBytes: 1024 * 1024 },
    makeEnvelope: (items: EnvelopeItem[]): Envelope =>
      ({
        header: { dsn: DSN, sdk: { name: 'sauron.javascript', version: 't' }, sent_at: '', release: null },
        context: {},
        items,
      }) as unknown as Envelope,
    fetchImpl,
    logger: { log: () => {}, warn: () => {} },
    onDisable: () => {
      harness.disabled = true;
    },
  });
  return harness;
}

/** Three distinguishable parked envelopes, oldest first. */
function park(transport: Transport, tags: string[]): void {
  for (const tag of tags) transport.offlineQueue.enqueue(`{"tag":"${tag}"}`);
}

function tagsIn(transport: Transport): string[] {
  return transport.offlineQueue.peek().map((e) => (JSON.parse(e) as { tag: string }).tag);
}

describe('drainOfflineQueue - a mid-drain failure must not eat the backlog', () => {
  beforeEach(() => {
    g.localStorage = new MemoryStorage();
  });

  afterEach(() => {
    if (originalLocalStorage === undefined) delete g.localStorage;
    else g.localStorage = originalLocalStorage;
  });

  it('keeps all three parked envelopes when the FIRST one 500s', async () => {
    const h = makeHarness([500]);
    park(h.transport, ['a', 'b', 'c']);
    expect(tagsIn(h.transport)).toEqual(['a', 'b', 'c']);

    await h.transport.drainOfflineQueue();

    // Exactly one attempt: draining stops at the first transient failure.
    expect(h.posted).toHaveLength(1);
    // `b` and `c` were never even tried — losing them is the regression.
    expect(tagsIn(h.transport)).toEqual(['a', 'b', 'c']);
  });

  it('keeps the untried remainder when a LATER envelope 500s', async () => {
    const h = makeHarness([202, 500]);
    park(h.transport, ['a', 'b', 'c']);

    await h.transport.drainOfflineQueue();

    expect(h.posted).toHaveLength(2); // a delivered, b failed, c untried
    expect(tagsIn(h.transport)).toEqual(['b', 'c']);
  });

  it('empties the queue when every envelope is accepted', async () => {
    const h = makeHarness([202]);
    park(h.transport, ['a', 'b', 'c']);

    await h.transport.drainOfflineQueue();

    expect(h.posted).toHaveLength(3);
    expect(tagsIn(h.transport)).toEqual([]);
  });

  it('keeps the backlog when the credentials are rejected mid-drain', async () => {
    // A 401 disables the client permanently, but the operator can fix the key and
    // re-init — throwing the backlog away here would make that unrecoverable.
    const h = makeHarness([202, 401]);
    park(h.transport, ['a', 'b', 'c']);

    await h.transport.drainOfflineQueue();

    expect(h.disabled).toBe(true);
    expect(h.transport.isEnabled()).toBe(false);
    expect(tagsIn(h.transport)).toEqual(['b', 'c']);
  });

  it('drops a 400 (non-retryable) and keeps draining the rest', async () => {
    const h = makeHarness([400, 202]);
    park(h.transport, ['a', 'b', 'c']);

    await h.transport.drainOfflineQueue();

    expect(h.posted).toHaveLength(3);
    expect(tagsIn(h.transport)).toEqual([]);
  });
});
