import { describe, it, expect, beforeEach } from 'vitest';

import { SauronClient } from '../src/client.js';
import type { Envelope, FetchLike } from '../src/types.js';

interface Captured {
  url: string;
  method: string;
  headers: Record<string, string>;
  envelope: Envelope;
}

/** A fake fetch that records every request and never touches the network. */
function makeFakeFetch(status = 200) {
  const calls: Captured[] = [];
  const fetchImpl: FetchLike = async (url, init) => {
    calls.push({
      url,
      method: init.method,
      headers: init.headers,
      envelope: JSON.parse(init.body) as Envelope,
    });
    return { status, ok: status >= 200 && status < 300 };
  };
  return { fetchImpl, calls };
}

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function newClient(fetchImpl: FetchLike, overrides = {}) {
  return new SauronClient({
    dsn: DSN,
    // Timer disabled so tests are deterministic — we flush manually.
    flushInterval: 0,
    fetchImpl,
    ...overrides,
  });
}

describe('transport POST', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('POSTs to {proto}://{host}/api/{project_id}/envelope with X-Sauron-Key', async () => {
    const client = newClient(fake.fetchImpl);
    client.track('signed_up', 'user-1');
    await client.flush();

    expect(fake.calls).toHaveLength(1);
    const call = fake.calls[0];
    expect(call.url).toBe('https://ingest.sauron.dev/api/99/envelope');
    expect(call.method).toBe('POST');
    expect(call.headers['X-Sauron-Key']).toBe('pub_key_abc');
    expect(call.headers['Content-Type']).toBe('application/json');
  });

  it('builds a header with the sauron-node SDK name/version', async () => {
    const client = newClient(fake.fetchImpl, { environment: 'staging', release: '1.2.3' });
    client.track('e', 'u');
    await client.flush();

    const { header, context } = fake.calls[0].envelope;
    expect(header.sdk).toEqual({ name: 'sauron-node', version: '0.1.0' });
    expect(header.dsn).toBe(DSN);
    expect(header.environment).toBe('staging');
    expect(header.release).toBe('1.2.3');
    expect(typeof header.sent_at).toBe('string');
    expect(context.runtime.name).toBe('node');
    expect(typeof context.device.device_id).toBe('string');
    expect(context.user).toBeNull();
  });

  it('emits a well-shaped event item', async () => {
    const client = newClient(fake.fetchImpl);
    client.track('purchase', 'user-42', { amount: 9.99, currency: 'USD' });
    await client.flush();

    const [item] = fake.calls[0].envelope.items;
    expect(item).toEqual({
      type: 'event',
      name: 'purchase',
      distinct_id: 'user-42',
      properties: { amount: 9.99, currency: 'USD' },
      timestamp: expect.any(String),
      session_id: null,
      screen: null,
    });
  });

  it('emits a well-shaped error item from captureException', async () => {
    const client = newClient(fake.fetchImpl);
    client.captureException(new TypeError('bad thing'), {
      user: { id: 'u1', email: 'a@b.co' },
      tags: { area: 'billing' },
      level: 'fatal',
    });
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.type).toBe('error');
    expect(typeof item.event_id).toBe('string');
    expect(item.level).toBe('fatal');
    expect(item.exception.type).toBe('TypeError');
    expect(item.exception.value).toBe('bad thing');
    expect(item.exception.mechanism).toEqual({ type: 'generic', handled: true });
    expect(Array.isArray(item.exception.stacktrace)).toBe(true);
    expect(item.exception.stacktrace.length).toBeGreaterThan(0);
    expect(item.tags).toEqual({ area: 'billing' });
    expect(item.user).toEqual({ id: 'u1', email: 'a@b.co', username: null });
    expect(item.message).toBeNull();
    expect(item.breadcrumbs).toEqual([]);
    expect(item.fingerprint).toBeNull();
    expect(item.session_id).toBeNull();
    expect(item.screen).toBeNull();
    // Crash frame is last.
    const frames = item.exception.stacktrace;
    expect(frames[frames.length - 1].in_app).toBe(true);
  });

  it('emits a well-shaped error item from captureMessage', async () => {
    const client = newClient(fake.fetchImpl);
    client.captureMessage('cache miss', 'warning');
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.type).toBe('error');
    expect(item.level).toBe('warning');
    expect(item.message).toBe('cache miss');
    expect(item.exception.type).toBe('Message');
    expect(item.exception.value).toBe('cache miss');
    expect(item.exception.stacktrace).toEqual([]);
  });

  it('emits a well-shaped identify item', async () => {
    const client = newClient(fake.fetchImpl);
    client.identify('user-7', { plan: 'pro' });
    await client.flush();

    const [item] = fake.calls[0].envelope.items;
    expect(item).toEqual({
      type: 'identify',
      distinct_id: 'user-7',
      anonymous_id: null,
      traits: { plan: 'pro' },
      timestamp: expect.any(String),
    });
  });
});

describe('batching', () => {
  it('does nothing on flush when the queue is empty', async () => {
    const fake = makeFakeFetch();
    const client = newClient(fake.fetchImpl);
    await client.flush();
    expect(fake.calls).toHaveLength(0);
  });

  it('flushes eagerly once maxBatch is reached', async () => {
    const fake = makeFakeFetch();
    const client = newClient(fake.fetchImpl, { maxBatch: 3 });
    client.track('a', 'u');
    client.track('b', 'u');
    expect(fake.calls).toHaveLength(0);
    client.track('c', 'u'); // hits maxBatch -> eager flush
    // Let the microtask from the eager async flush settle.
    await client.flush();
    expect(fake.calls).toHaveLength(1);
    expect(fake.calls[0].envelope.items).toHaveLength(3);
  });

  it('batches multiple items into a single envelope', async () => {
    const fake = makeFakeFetch();
    const client = newClient(fake.fetchImpl);
    client.track('a', 'u');
    client.identify('u', { plan: 'free' });
    client.captureMessage('hi');
    await client.flush();
    expect(fake.calls).toHaveLength(1);
    expect(fake.calls[0].envelope.items).toHaveLength(3);
  });
});

describe('failure handling', () => {
  it('disables the SDK on a 401 and stops sending', async () => {
    const fake = makeFakeFetch(401);
    const client = newClient(fake.fetchImpl);
    client.track('a', 'u');
    await client.flush();
    expect(fake.calls).toHaveLength(1);

    // Further items are dropped; no further POSTs.
    client.track('b', 'u');
    await client.flush();
    expect(fake.calls).toHaveLength(1);
  });

  it('swallows a rejected fetch without throwing', async () => {
    const boom: FetchLike = async () => {
      throw new Error('network down');
    };
    const client = newClient(boom);
    client.track('a', 'u');
    await expect(client.flush()).resolves.toBeUndefined();
  });

  it('drops invalid track calls (missing distinct_id)', async () => {
    const fake = makeFakeFetch();
    const client = newClient(fake.fetchImpl);
    client.track('a', '');
    await client.flush();
    expect(fake.calls).toHaveLength(0);
  });
});

describe('close', () => {
  it('flushes on close', async () => {
    const fake = makeFakeFetch();
    const client = newClient(fake.fetchImpl);
    client.track('a', 'u');
    await client.close();
    expect(fake.calls).toHaveLength(1);
  });
});
