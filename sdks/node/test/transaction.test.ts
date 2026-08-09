import { describe, it, expect, beforeEach } from 'vitest';

import { SauronClient } from '../src/client.js';
import { withScope } from '../src/scope.js';
import type { Envelope, FetchLike } from '../src/types.js';
import { bodyToString } from './helpers.js';

interface Captured {
  envelope: Envelope;
}

function makeFakeFetch() {
  const calls: Captured[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    calls.push({ envelope: JSON.parse(bodyToString(init)) as Envelope });
    return { status: 200, ok: true };
  };
  return { fetchImpl, calls };
}

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function newClient(fetchImpl: FetchLike) {
  return new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl });
}

describe('trackTransaction', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('emits a transaction item with the given fields', async () => {
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({
      name: 'GET /u',
      op: 'http',
      duration_ms: 12.5,
      http_status: 200,
      http_method: 'GET',
      url: '/u',
      status: 'ok',
    });
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.type).toBe('transaction');
    expect(item.name).toBe('GET /u');
    expect(item.op).toBe('http');
    expect(item.duration_ms).toBe(12.5);
    expect(item.http_status).toBe(200);
    expect(item.http_method).toBe('GET');
    expect(item.url).toBe('/u');
    expect(item.status).toBe('ok');
    expect(typeof item.timestamp).toBe('string');
  });

  it('defaults op to "custom" when omitted', async () => {
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({ name: 'work', duration_ms: 5 });
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.op).toBe('custom');
    // Absent optional fields must not leak as null.
    expect('http_status' in item).toBe(false);
    expect('distinct_id' in item).toBe(false);
  });

  it('falls back distinct_id to the scoped user id', async () => {
    const client = newClient(fake.fetchImpl);
    await withScope(async (s) => {
      s.setUser({ id: 'u9' });
      client.trackTransaction({ name: 'work', duration_ms: 5 });
      await client.flush();
    });

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.distinct_id).toBe('u9');
  });

  // The browser SDK's equivalent input takes `durationMs`, so a snippet ported
  // between the two — or any plain-JS caller — used to ship a transaction whose
  // duration field was simply absent, with no error anywhere. A reviewer
  // reproduced it against the built SDK.
  it('accepts durationMs, the browser SDK spelling, as an alias', async () => {
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({ name: 'ported', durationMs: 42 } as never);
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.duration_ms).toBe(42);
  });

  it('prefers duration_ms when both spellings are supplied', async () => {
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({ name: 'both', duration_ms: 7, durationMs: 99 } as never);
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.duration_ms).toBe(7);
  });

  it('drops a transaction with no usable duration instead of shipping it', async () => {
    // Refusing is the point. A transaction whose whole purpose is to record a
    // duration is not worth persisting without one, and sending it anyway is
    // what let the misspelling go unnoticed — the item looked delivered.
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({ name: 'no-duration' } as never);
    client.trackTransaction({ name: 'nan', duration_ms: Number.NaN });
    client.trackTransaction({ name: 'infinite', duration_ms: Number.POSITIVE_INFINITY });
    client.trackTransaction({ name: 'stringy', duration_ms: '12' } as never);
    await client.flush();

    expect(fake.calls).toHaveLength(0);
  });

  it('still emits when the duration is zero', async () => {
    // `0` is a legitimate duration and must not be caught by a truthiness test.
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({ name: 'instant', duration_ms: 0 });
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.duration_ms).toBe(0);
  });

  it('prefers an explicit distinct_id over the scoped user', async () => {
    const client = newClient(fake.fetchImpl);
    await withScope(async (s) => {
      s.setUser({ id: 'u9' });
      client.trackTransaction({ name: 'work', duration_ms: 5, distinct_id: 'explicit' });
      await client.flush();
    });

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.distinct_id).toBe('explicit');
  });
});
