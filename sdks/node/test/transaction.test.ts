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
