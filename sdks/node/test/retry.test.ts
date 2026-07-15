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
  let i = 0;
  const fetchImpl: FetchLike = async () => {
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
  return { fetchImpl, statuses, calls: () => i };
}

function makeTransport(fetchImpl: FetchLike, sleeps: number[], overrides = {}) {
  return new Transport({
    dsn: parseDsn(DSN),
    environment: 'test',
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

  it('retries 408, 413 and 5xx', async () => {
    for (const code of [408, 413, 500, 502, 503]) {
      const sleeps: number[] = [];
      const script = scriptedFetch([code, 200]);
      const t = makeTransport(script.fetchImpl, sleeps);
      t.enqueue(evt());
      await t.flush();
      expect(script.calls(), `status ${code} should retry`).toBe(2);
    }
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
