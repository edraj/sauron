import { describe, it, expect } from 'vitest';

import { Transport } from '../src/transport.js';
import { parseDsn } from '../src/dsn.js';
import type { Context, EnvelopeItem, FetchLike } from '../src/types.js';

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function ctx(): Context {
  return {
    device: { device_id: 'd1' },
    os: { name: 'linux', version: '1' },
    app: {},
    runtime: { name: 'node', version: '20' },
    user: null,
  };
}

function evt(name = 'e'): EnvelopeItem {
  return {
    type: 'event',
    name,
    distinct_id: 'u1',
    properties: {},
    timestamp: new Date().toISOString(),
    session_id: null,
    screen: null,
  };
}

interface HeaderMap {
  [k: string]: string;
}

/** Build a fetch that yields a queued sequence of responses. */
function scriptedFetch(script: Array<number | { status: number; retryAfter?: string } | 'throw'>) {
  const statuses: number[] = [];
  /** Items carried by each request, so tests can assert envelope sizes. */
  const sizes: number[] = [];
  let i = 0;
  const fetchImpl: FetchLike = async (_url: string, init?: { body?: unknown }) => {
    const body = init?.body;
    if (typeof body === 'string') {
      try {
        sizes.push((JSON.parse(body).items as unknown[]).length);
      } catch {
        /* gzipped or unparseable — size assertions opt out via gzipThresholdBytes */
      }
    }
    const step = script[Math.min(i, script.length - 1)];
    i += 1;
    if (step === 'throw') {
      statuses.push(-1);
      throw new Error('network down');
    }
    const spec = typeof step === 'number' ? { status: step } : step;
    statuses.push(spec.status);
    const headers: HeaderMap = {};
    if (spec.retryAfter !== undefined) headers['retry-after'] = spec.retryAfter;
    return {
      status: spec.status,
      ok: spec.status >= 200 && spec.status < 300,
      headers: { get: (name: string) => headers[name.toLowerCase()] ?? null },
    };
  };
  return { fetchImpl, statuses, sizes, calls: () => i };
}

function makeTransport(fetchImpl: FetchLike, sleeps: number[], overrides = {}) {
  return new Transport({
    dsn: parseDsn(DSN),
    release: null,
    context: ctx(),
    flushInterval: 0,
    maxBatch: 30,
    fetchImpl,
    debug: false,
    sleep: async (ms: number) => {
      sleeps.push(ms);
    },
    ...overrides,
  });
}

describe('retry policy', () => {
  it('retries a 429 (honoring Retry-After) then succeeds — 2 calls total', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([{ status: 429, retryAfter: '0' }, 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(2);
    expect(sleeps).toEqual([0]); // Retry-After: 0 → immediate retry
  });

  it('honors a numeric Retry-After in seconds', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([{ status: 429, retryAfter: '2' }, 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(2);
    expect(sleeps).toEqual([2000]);
  });

  it('drops immediately on a 400 with no retry — 1 call total', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([400, 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(1);
    expect(sleeps).toEqual([]);
  });

  it('drops (no retry) on 404', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([404, 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(1);
  });

  it('retries 408 and 5xx', async () => {
    for (const code of [408, 500, 502, 503]) {
      const sleeps: number[] = [];
      const script = scriptedFetch([code, 200]);
      const t = makeTransport(script.fetchImpl, sleeps);
      t.enqueue(evt());
      await t.flush();
      expect(script.calls(), `status ${code} should retry`).toBe(2);
    }
  });

  // 413 is deliberately NOT retried as-is: the body is what the server
  // rejected, so an identical retry can only fail identically. A full queue
  // used to wedge the transport permanently that way.
  it('shrinks and re-buffers on 413 instead of retrying the same body', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([413, 200, 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    t.enqueue(evt());
    await t.flush();
    // One rejected attempt, not two identical ones.
    expect(script.calls(), '413 must not retry the same body').toBe(1);
    // The items were kept, so a later flush delivers them.
    await t.flush();
    expect(script.calls()).toBeGreaterThan(1);
  });

  it('drops a single item that is too large, rather than looping forever', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([413, 413, 413, 413]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    await t.flush();
    // Second flush finds an empty queue: the unsendable item was discarded.
    expect(script.calls()).toBe(1);
  });

  it('retries a network error then succeeds', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch(['throw', 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(2);
    expect(sleeps).toHaveLength(1);
  });

  it('gives up after maxRetries on a persistent 500 and stops calling', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([500]); // always 500
    const t = makeTransport(script.fetchImpl, sleeps, { maxRetries: 3 });
    t.enqueue(evt());
    await t.flush();
    // 1 initial attempt + 3 retries = 4 calls, then give up.
    expect(script.calls()).toBe(4);
    expect(sleeps).toHaveLength(3);
  });

  it('caps each backoff sleep at 30s', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([500]);
    const t = makeTransport(script.fetchImpl, sleeps, {
      maxRetries: 3,
      retryBaseMs: 100_000, // absurdly large base so the cap must clamp it
    });
    t.enqueue(evt());
    await t.flush();
    for (const ms of sleeps) expect(ms).toBeLessThanOrEqual(30_000);
  });

  it('disables the SDK on 401 without retrying', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([401, 200]);
    const t = makeTransport(script.fetchImpl, sleeps);
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(1);
    // Disabled: further enqueue + flush sends nothing more.
    t.enqueue(evt());
    await t.flush();
    expect(script.calls()).toBe(1);
  });
});

// The server rejects an envelope carrying more than 1000 items with a
// non-retryable 400, which commits the batch and unlinks its persisted files —
// so an uncapped drain could destroy a whole offline backlog in one request.
describe('envelope item cap', () => {
  it('splits a backlog larger than the cap across several envelopes', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([200]);
    const t = makeTransport(script.fetchImpl, sleeps, {
      maxQueueBytes: 64 * 1024 * 1024,
      maxItemsPerEnvelope: 100,
      gzipThresholdBytes: Number.MAX_SAFE_INTEGER,
    });
    for (let i = 0; i < 250; i += 1) t.enqueue(evt(`e${i}`));
    await t.flush();
    // 250 items at 100 per envelope → 3 requests, none oversized.
    expect(script.calls()).toBe(3);
    expect(script.sizes.every((n) => n <= 100)).toBe(true);
    expect(script.sizes.reduce((a, b) => a + b, 0)).toBe(250);
  });

  it('never exceeds the cap even when maxBatch is larger', async () => {
    const sleeps: number[] = [];
    const script = scriptedFetch([200]);
    const t = makeTransport(script.fetchImpl, sleeps, {
      maxQueueBytes: 64 * 1024 * 1024,
      maxBatch: 5000,
      maxItemsPerEnvelope: 1000,
      gzipThresholdBytes: Number.MAX_SAFE_INTEGER,
    });
    for (let i = 0; i < 1500; i += 1) t.enqueue(evt(`e${i}`));
    await t.flush();
    expect(Math.max(...script.sizes)).toBeLessThanOrEqual(1000);
  });
});
