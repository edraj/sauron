import { describe, it, expect, beforeEach } from 'vitest';
import { SauronClient } from '../src/client.js';
import { getGlobalScope } from '../src/scope.js';
import type { Envelope, ErrorItem, EventItem, FetchLike } from '../src/types.js';
import { bodyToString } from './helpers.js';

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function makeFakeFetch() {
  const envelopes: Envelope[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    envelopes.push(JSON.parse(bodyToString(init)) as Envelope);
    return { status: 200, ok: true };
  };
  return { fetchImpl, envelopes };
}

beforeEach(() => {
  // Global scope is process-wide; reset it so seeding/per-call tests don't leak.
  const g = getGlobalScope().data;
  g.user = null;
  g.tags = {};
  g.contexts = {};
  g.extra = {};
  g.breadcrumbs = [];
});

describe('metadata wire types', () => {
  it('ErrorItem carries optional contexts/extra that round-trip through JSON', () => {
    const item: ErrorItem = {
      type: 'error',
      event_id: 'evt_1',
      level: 'error',
      timestamp: 'TS',
      exception: {
        type: 'E',
        value: null,
        mechanism: { type: 'generic', handled: true },
        stacktrace: [],
      },
      message: null,
      breadcrumbs: [],
      tags: { a: '1' },
      contexts: { order: { id: 7 } },
      extra: { trace_id: 'abc' },
      fingerprint: null,
      user: null,
      session_id: null,
      screen: null,
    };
    expect(JSON.parse(JSON.stringify(item))).toEqual(item);
  });

  it('EventItem carries optional tags/contexts/extra that round-trip through JSON', () => {
    const item: EventItem = {
      type: 'event',
      name: 'checkout',
      distinct_id: 'u_1',
      properties: {},
      timestamp: 'TS',
      session_id: null,
      screen: null,
      tags: { plan: 'pro' },
      contexts: { order: { id: 7 } },
      extra: { trace_id: 'abc' },
    };
    expect(JSON.parse(JSON.stringify(item))).toEqual(item);
  });
});

describe('init seeds the global scope with default metadata', () => {
  it('applies init tags/contexts/extra to captured errors and tracked events', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({
      dsn: DSN,
      flushInterval: 0,
      fetchImpl: fake.fetchImpl,
      tags: { env: 'prod' },
      contexts: { order: { id: 7 } }, // dev contexts — NOT the machine `context` (device/os/app/runtime/user)
      extra: { region: 'eu' },
    });
    client.captureMessage('hello');
    client.track('viewed', 'u_1');
    await client.flush();

    const items = fake.envelopes.flatMap((e) => e.items) as Record<string, any>[];
    const error = items.find((i) => i.type === 'error')!;
    const event = items.find((i) => i.type === 'event')!;
    expect(error.tags).toEqual({ env: 'prod' });
    expect(error.contexts).toEqual({ order: { id: 7 } });
    expect(error.extra).toEqual({ region: 'eu' });
    expect(event.tags).toEqual({ env: 'prod' });
    expect(event.contexts).toEqual({ order: { id: 7 } });
    expect(event.extra).toEqual({ region: 'eu' });
    await client.close();
  });
});

describe('per-call metadata overrides scope', () => {
  it('captureException per-call contexts/extra override same-named scope blocks', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    getGlobalScope().setContext('order', { id: 1 });
    getGlobalScope().setExtra('trace_id', 'scope');
    client.captureException(new Error('boom'), {
      contexts: { order: { id: 2 } },
      extra: { call_key: 'call' },
    });
    await client.flush();
    const item = fake.envelopes[0].items[0] as Record<string, any>;
    expect(item.contexts).toEqual({ order: { id: 2 } }); // per-call block replaced scope's
    expect(item.extra).toEqual({ trace_id: 'scope', call_key: 'call' }); // shallow-merged by key
    await client.close();
  });

  it('captureMessage accepts per-call tags/contexts/extra', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    client.captureMessage('note', 'warning', {
      tags: { a: '1' },
      contexts: { page: { route: '/x' } },
    });
    await client.flush();
    const item = fake.envelopes[0].items[0] as Record<string, any>;
    expect(item.level).toBe('warning');
    expect(item.tags).toEqual({ a: '1' });
    expect(item.contexts).toEqual({ page: { route: '/x' } });
    expect('extra' in item).toBe(false); // omit-when-empty
    await client.close();
  });

  it('track merges per-call tags over scope and omits empty contexts/extra', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    getGlobalScope().setTag('env', 'prod');
    client.track('viewed', 'u_1', {}, { tags: { plan: 'pro' } });
    await client.flush();
    const item = fake.envelopes[0].items[0] as Record<string, any>;
    expect(item.tags).toEqual({ env: 'prod', plan: 'pro' });
    expect('contexts' in item).toBe(false);
    expect('extra' in item).toBe(false);
    await client.close();
  });
});
