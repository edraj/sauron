import { describe, expect, it } from 'vitest';
import { buildEnvelope } from '../src/envelope';
import { buildTransactionItem } from '../src/api/product';
import { parseDsn } from '../src/dsn';
import type { Envelope, TransactionItem } from '../src/types';

/**
 * Golden envelope from the LOCKED wire contract. The Rust ingest gateway and the
 * Flutter SDK emit/consume this identical shape, so it must match byte-for-byte.
 *
 * `context.device.device_id` is the durable device identity; `session_id` is
 * attached to every event, error and transaction item.
 */
const GOLDEN: Envelope = {
  header: {
    dsn: 'https://pk_test@localhost:8081/1',
    sdk: { name: 'sauron.javascript', version: '0.1.0' },
    sent_at: '2026-07-12T10:30:00.123Z',
    environment: 'production',
    release: 'web@1.4.2',
  },
  context: {
    device: {
      device_id: '4f9a1c2b-3d4e-4a5f-8b6c-7d8e9f0a1b2c',
      family: 'Apple',
      model: null,
      arch: null,
    },
    os: { name: 'macOS', version: '14.5' },
    app: { version: '1.4.2', build: null },
    runtime: { name: 'Chrome', version: '126' },
    user: { id: 'u_123', email: null, traits: {} },
  },
  items: [
    {
      type: 'error',
      timestamp: '2026-07-12T10:29:58.900Z',
      level: 'error',
      exception: {
        type: 'TypeError',
        value: 'x is not a function',
        mechanism: { type: 'onunhandledrejection', handled: false },
        stacktrace: [
          { function: 'loadUser', filename: 'app.js', lineno: 42, colno: 13, in_app: true },
        ],
      },
      breadcrumbs: [
        {
          type: 'navigation',
          category: 'history',
          message: null,
          level: 'info',
          timestamp: '2026-07-12T10:29:50.000Z',
          data: { from: '/', to: '/settings' },
        },
      ],
      fingerprint: null,
      session_id: 'sess_abc123',
    },
    {
      type: 'event',
      name: 'checkout_completed',
      distinct_id: 'u_123',
      session_id: 'sess_abc123',
      timestamp: '2026-07-12T10:29:40.000Z',
      properties: { cart_value: 42.5 },
    },
    {
      type: 'identify',
      distinct_id: 'u_123',
      anonymous_id: null,
      traits: { plan: 'pro' },
    },
  ],
};

describe('buildEnvelope', () => {
  it('produces the golden envelope shape exactly', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    expect(built).toEqual(GOLDEN);
  });

  it('preserves key ordering and nullability through JSON round-trip', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    const roundTripped = JSON.parse(JSON.stringify(built));
    expect(roundTripped).toEqual(GOLDEN);
  });

  it('keeps the canonical top-level key order header/context/items', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    expect(Object.keys(built)).toEqual(['header', 'context', 'items']);
  });

  it('carries the locked SDK identity in the header', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    expect(built.header.sdk).toEqual({ name: 'sauron.javascript', version: '0.1.0' });
  });

  it('uses the discriminated item types error/event/identify', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    expect(built.items.map((i) => i.type)).toEqual(['error', 'event', 'identify']);
  });

  it('carries the durable device_id on the device context', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    expect(built.context.device.device_id).toBe('4f9a1c2b-3d4e-4a5f-8b6c-7d8e9f0a1b2c');
  });

  it('attaches session_id to error and event items', () => {
    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, GOLDEN.items);
    const error = built.items.find((i) => i.type === 'error');
    const event = built.items.find((i) => i.type === 'event');
    expect(error && 'session_id' in error && error.session_id).toBe('sess_abc123');
    expect(event && 'session_id' in event && event.session_id).toBe('sess_abc123');
  });
});

describe('transaction item', () => {
  it('serializes to the locked transaction wire shape', () => {
    const tx: TransactionItem = {
      type: 'transaction',
      name: 'GET /api/users',
      op: 'http',
      duration_ms: 128.4,
      status: 'ok',
      http_method: 'GET',
      http_status: 200,
      url: 'https://api.example.com/api/users',
      distinct_id: 'u_123',
      session_id: 'sess_abc123',
      timestamp: '2026-07-12T10:29:41.000Z',
    };

    const built = buildEnvelope(GOLDEN.header, GOLDEN.context, [tx]);
    const roundTripped = JSON.parse(JSON.stringify(built));
    expect(roundTripped.items[0]).toEqual(tx);
    expect(roundTripped.items[0].type).toBe('transaction');
  });

  it('builds a normalized transaction from camelCase input', () => {
    const item = buildTransactionItem(
      {
        name: 'checkout',
        op: 'http',
        durationMs: 42,
        httpMethod: 'POST',
        httpStatus: 201,
        url: 'https://api.example.com/checkout',
      },
      'u_123',
      'sess_abc123',
    );

    expect(item).toEqual({
      type: 'transaction',
      name: 'checkout',
      op: 'http',
      duration_ms: 42,
      status: null,
      http_method: 'POST',
      http_status: 201,
      url: 'https://api.example.com/checkout',
      distinct_id: 'u_123',
      session_id: 'sess_abc123',
      timestamp: item.timestamp,
    });
    expect(typeof item.timestamp).toBe('string');
  });

  it('coerces an unknown op to "custom" and fills absent fields with null', () => {
    const item = buildTransactionItem(
      { name: 'render', op: 'not_a_real_op', durationMs: 7 },
      null,
      null,
    );
    expect(item.op).toBe('custom');
    expect(item.status).toBeNull();
    expect(item.http_method).toBeNull();
    expect(item.http_status).toBeNull();
    expect(item.url).toBeNull();
    expect(item.distinct_id).toBeNull();
    expect(item.session_id).toBeNull();
  });
});

describe('parseDsn', () => {
  it('derives transport URLs from the golden DSN', () => {
    const dsn = parseDsn('https://pk_test@localhost:8081/1');
    expect(dsn.publicKey).toBe('pk_test');
    expect(dsn.host).toBe('localhost:8081');
    expect(dsn.hostname).toBe('localhost');
    expect(dsn.protocol).toBe('https');
    expect(dsn.projectId).toBe('1');
    expect(dsn.envelopeUrl).toBe('https://localhost:8081/api/1/envelope');
    expect(dsn.beaconUrl).toBe('https://localhost:8081/api/1/envelope?k=pk_test');
    expect(dsn.raw).toBe('https://pk_test@localhost:8081/1');
  });

  it('rejects a DSN without a public key', () => {
    expect(() => parseDsn('https://localhost:8081/1')).toThrow();
  });

  it('rejects a DSN carrying a secret', () => {
    expect(() => parseDsn('https://pk_test:secret@localhost:8081/1')).toThrow();
  });

  it('rejects a DSN without a project id', () => {
    expect(() => parseDsn('https://pk_test@localhost:8081/')).toThrow();
  });
});
