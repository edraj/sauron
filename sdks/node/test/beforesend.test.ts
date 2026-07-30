import { describe, it, expect, beforeEach } from 'vitest';

import { SauronClient } from '../src/client.js';
import type { Envelope, EnvelopeItem, FetchLike, InitOptions } from '../src/types.js';
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

function newClient(fetchImpl: FetchLike, overrides: Partial<InitOptions> = {}) {
  return new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl, ...overrides });
}

describe('beforeSend', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('mutates an outgoing event item (PII redaction)', async () => {
    const beforeSend = (item: EnvelopeItem): EnvelopeItem | null => {
      if (item.type === 'event' && item.properties && 'email' in item.properties) {
        item.properties.email = '[redacted]';
      }
      return item;
    };
    const client = newClient(fake.fetchImpl, { beforeSend });
    client.track('signup', 'u1', { email: 'a@b.co', plan: 'pro' });
    await client.flush();

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.type).toBe('event');
    expect(item.properties.email).toBe('[redacted]');
    expect(item.properties.plan).toBe('pro');
  });

  it('drops an item when beforeSend returns null', async () => {
    const beforeSend = (item: EnvelopeItem): EnvelopeItem | null =>
      item.type === 'error' ? null : item;
    const client = newClient(fake.fetchImpl, { beforeSend });
    client.track('kept', 'u1');
    client.captureException(new Error('boom'));
    await client.flush();

    expect(fake.calls).toHaveLength(1);
    const types = fake.calls[0].envelope.items.map((i) => i.type);
    expect(types).toEqual(['event']);
  });

  it('runs on every item type including transactions', async () => {
    const seen: string[] = [];
    const beforeSend = (item: EnvelopeItem): EnvelopeItem | null => {
      seen.push(item.type);
      return item;
    };
    const client = newClient(fake.fetchImpl, { beforeSend });
    client.track('e', 'u1');
    client.identify('u1', { plan: 'pro' });
    client.captureMessage('note');
    client.trackTransaction({ name: 'GET /x', duration_ms: 3 });
    await client.flush();

    expect(seen.sort()).toEqual(['error', 'event', 'identify', 'transaction']);
  });

  it('a throwing beforeSend does not propagate into the caller, and the item is still sent unmodified', async () => {
    // The SDK's no-throw guarantee: a user-supplied hook blowing up must
    // never break the caller's captureException/track/etc, and telemetry
    // must not be silently dropped just because the hook errored.
    const beforeSend = (): EnvelopeItem | null => {
      throw new Error('customer hook exploded');
    };
    const client = newClient(fake.fetchImpl, { beforeSend });

    expect(() => client.captureException(new Error('boom'))).not.toThrow();
    expect(() => client.track('signup', 'u1', { plan: 'pro' })).not.toThrow();
    await client.flush();

    expect(fake.calls).toHaveLength(1);
    const types = fake.calls[0].envelope.items.map((i) => i.type);
    expect(types.sort()).toEqual(['error', 'event']);
    const eventItem = fake.calls[0].envelope.items.find((i) => i.type === 'event') as any;
    expect(eventItem.name).toBe('signup');
    expect(eventItem.properties.plan).toBe('pro');
  });
});
