import { describe, it, expect, beforeEach } from 'vitest';

import { SauronClient } from '../src/client.js';
import { withScope } from '../src/scope.js';
import type {
  Envelope,
  ErrorItem,
  EventItem,
  FetchLike,
  IdentifyItem,
  InitOptions,
  TransactionItem,
} from '../src/types.js';
import { bodyToString } from './helpers.js';

/**
 * The shared golden envelope every SDK must emit byte-compatibly with the Rust
 * ingest contract (`backend/crates/sauron-core/src/envelope.rs`). Extended for
 * the server SDKs to a *server-shaped error item* (real breadcrumbs + tags +
 * user + fingerprint), an event, an identify, and a transaction — the reconciled
 * shape guarded across Node/Python/C#.
 *
 * Field names are snake_case on the wire (`distinct_id`, `event_id`,
 * `http_status`, `duration_ms`, `http_method`); item `type` tags are
 * `error | event | identify | transaction`.
 */
const GOLDEN_ERROR: ErrorItem = {
  type: 'error',
  event_id: 'evt_0000000000000001',
  level: 'error',
  timestamp: '2026-07-15T10:29:58.900Z',
  exception: {
    type: 'TypeError',
    value: 'x is not a function',
    mechanism: { type: 'generic', handled: true },
    stacktrace: [
      {
        function: 'loadUser',
        module: null,
        filename: 'app.js',
        abs_path: null,
        lineno: 42,
        colno: 13,
        in_app: true,
      },
    ],
  },
  message: null,
  breadcrumbs: [
    {
      type: 'navigation',
      category: 'history',
      message: null,
      level: 'info',
      timestamp: '2026-07-15T10:29:50.000Z',
      data: { from: '/', to: '/settings' },
    },
  ],
  tags: { env: 'prod', req: '42' },
  contexts: { order: { id: 7 } },
  extra: { trace_id: 'abc123' },
  fingerprint: ['checkout-failure'],
  user: { id: 'u_123', email: null, username: null },
  session_id: null,
  screen: null,
};

const GOLDEN_EVENT: EventItem = {
  type: 'event',
  name: 'checkout_completed',
  distinct_id: 'u_123',
  properties: { cart_value: 42.5 },
  timestamp: '2026-07-15T10:29:40.000Z',
  session_id: null,
  screen: null,
  tags: { env: 'prod', req: '42' },
  contexts: { order: { id: 7 } },
  extra: { trace_id: 'abc123' },
};

const GOLDEN_IDENTIFY: IdentifyItem = {
  type: 'identify',
  distinct_id: 'u_123',
  anonymous_id: null,
  traits: { plan: 'pro' },
  timestamp: '2026-07-15T10:29:39.000Z',
};

const GOLDEN_TRANSACTION: TransactionItem = {
  type: 'transaction',
  name: 'GET /api/users',
  op: 'http',
  duration_ms: 128.4,
  status: 'ok',
  http_method: 'GET',
  http_status: 200,
  url: '/api/users',
  distinct_id: 'u_123',
  timestamp: '2026-07-15T10:29:41.000Z',
};

const GOLDEN: Envelope = {
  header: {
    dsn: 'https://pk_test@localhost:8081/1',
    sdk: { name: 'sauron-node', version: '0.3.0' },
    sent_at: '2026-07-15T10:30:00.123Z',
    environment: 'production',
    release: 'api@1.4.2',
  },
  context: {
    device: { device_id: '4f9a1c2b-3d4e-4a5f-8b6c-7d8e9f0a1b2c' },
    os: { name: 'linux', version: '6.0.0' },
    app: {},
    runtime: { name: 'node', version: '20.0.0' },
    user: null,
  },
  items: [GOLDEN_ERROR, GOLDEN_EVENT, GOLDEN_IDENTIFY, GOLDEN_TRANSACTION],
};

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function makeFakeFetch() {
  const envelopes: Envelope[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    envelopes.push(JSON.parse(bodyToString(init)) as Envelope);
    return { status: 200, ok: true };
  };
  return { fetchImpl, envelopes };
}

function newClient(fetchImpl: FetchLike, overrides: Partial<InitOptions> = {}) {
  return new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl, ...overrides });
}

/**
 * Blank out the volatile parts an SDK can't reproduce deterministically
 * (generated ids, wall-clock timestamps, symbolication-dependent frames) so the
 * *shape* — keys, nesting, snake_case, nullability — is what gets asserted.
 */
function normalize(item: Record<string, any>): Record<string, any> {
  const clone = JSON.parse(JSON.stringify(item));
  if ('event_id' in clone) clone.event_id = 'EVENT_ID';
  if ('timestamp' in clone) clone.timestamp = 'TS';
  if (clone.exception && Array.isArray(clone.exception.stacktrace)) {
    clone.exception.stacktrace = 'STACK';
  }
  if (Array.isArray(clone.breadcrumbs)) {
    for (const crumb of clone.breadcrumbs) crumb.timestamp = 'TS';
  }
  return clone;
}

describe('golden envelope fixture', () => {
  it('round-trips the golden shape byte-for-byte through JSON', () => {
    expect(JSON.parse(JSON.stringify(GOLDEN))).toEqual(GOLDEN);
  });

  it('tags each item with its snake_case discriminated type', () => {
    expect(GOLDEN.items.map((i) => i.type)).toEqual([
      'error',
      'event',
      'identify',
      'transaction',
    ]);
  });

  it('uses snake_case wire keys on the transaction item', () => {
    const keys = Object.keys(GOLDEN_TRANSACTION);
    expect(keys).toContain('duration_ms');
    expect(keys).toContain('http_method');
    expect(keys).toContain('http_status');
    expect(keys).toContain('distinct_id');
  });
});

describe('client emits the reconciled golden shape', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('produces items matching the golden fixture (volatile fields aside)', async () => {
    const client = newClient(fake.fetchImpl);

    await withScope(async (scope) => {
      scope.setUser({ id: 'u_123' });
      scope.setTags({ env: 'prod', req: '42' });
      scope.setContext('order', { id: 7 });
      scope.setExtra('trace_id', 'abc123');
      client.addBreadcrumb({
        type: 'navigation',
        category: 'history',
        level: 'info',
        data: { from: '/', to: '/settings' },
      });
      client.captureException(new TypeError('x is not a function'), {
        fingerprint: ['checkout-failure'],
      });
      client.track('checkout_completed', 'u_123', { cart_value: 42.5 });
      client.identify('u_123', { plan: 'pro' });
      client.trackTransaction({
        name: 'GET /api/users',
        op: 'http',
        duration_ms: 128.4,
        status: 'ok',
        http_method: 'GET',
        http_status: 200,
        url: '/api/users',
      });
      await client.flush();
    });

    const items = fake.envelopes.flatMap((e) => e.items) as Record<string, any>[];
    expect(items.map((i) => i.type)).toEqual(['error', 'event', 'identify', 'transaction']);
    expect(items.map(normalize)).toEqual(GOLDEN.items.map((i) => normalize(i as any)));
  });

  it('carries the bumped 0.3.0 sauron-node SDK identity in the header', async () => {
    const client = newClient(fake.fetchImpl);
    client.captureMessage('hello');
    await client.flush();

    expect(fake.envelopes[0].header.sdk).toEqual({ name: 'sauron-node', version: '0.3.0' });
  });

  it('stamps a real event_id and ISO timestamp on the captured error', async () => {
    const client = newClient(fake.fetchImpl);
    client.captureException(new Error('boom'), { fingerprint: ['grp'] });
    await client.flush();

    const item = fake.envelopes[0].items[0] as any;
    expect(item.type).toBe('error');
    expect(typeof item.event_id).toBe('string');
    expect(item.event_id.length).toBeGreaterThan(0);
    expect(new Date(item.timestamp).toISOString()).toBe(item.timestamp);
    expect(item.fingerprint).toEqual(['grp']);
    expect(Array.isArray(item.exception.stacktrace)).toBe(true);
    expect(item.exception.stacktrace.length).toBeGreaterThan(0);
  });

  it('omits absent transaction optionals rather than leaking null', async () => {
    const client = newClient(fake.fetchImpl);
    client.trackTransaction({ name: 'work', duration_ms: 5 });
    await client.flush();

    const item = fake.envelopes[0].items[0] as unknown as Record<string, unknown>;
    expect(item.type).toBe('transaction');
    expect('http_status' in item).toBe(false);
    expect('http_method' in item).toBe(false);
    expect('status' in item).toBe(false);
    expect('url' in item).toBe(false);
    expect('distinct_id' in item).toBe(false);
  });

  it('omits contexts/extra on an error captured with no metadata set', async () => {
    const client = newClient(fake.fetchImpl);
    client.captureException(new Error('bare'));
    await client.flush();

    const item = fake.envelopes[0].items[0] as unknown as Record<string, unknown>;
    expect(item.type).toBe('error');
    expect('contexts' in item).toBe(false);
    expect('extra' in item).toBe(false);
    expect(item.tags).toEqual({}); // tags stays present per the existing Node convention
  });
});
