import { describe, expect, it } from 'vitest';

import { SauronClient } from '../src/client.js';
import { getCurrentScope } from '../src/scope.js';
import type { FetchLike } from '../src/types.js';
import { bodyToString } from './helpers.js';
import { writeWireFixture } from './wire-fixture-io.js';

/**
 * Captures the envelope this SDK **actually posts** and writes it to
 * `sdks/wire-fixtures/node.json`, where the backend's
 * `sauron-core --test sdk_wire_conformance` feeds it through the real `serde`
 * deserializer.
 *
 * The golden literal in `envelope.test.ts` is authored in this repo, so it can
 * (and once did, for `js`) agree perfectly with an item shape the gateway
 * rejects outright — a 400 `invalid_envelope` that takes the whole batch with
 * it. This fixture is the posted body, so only the gateway's opinion matters.
 */
describe('wire fixture (node)', () => {
  it('posts an envelope that is captured verbatim into sdks/wire-fixtures/node.json', async () => {
    const bodies: Array<{ headers: Record<string, string>; body: string | Uint8Array }> = [];
    const fetchImpl: FetchLike = async (_url, init) => {
      bodies.push({ headers: init.headers, body: init.body });
      return { status: 202, headers: { get: () => null } };
    };

    const client = new SauronClient({
      dsn: 'https://pk_test@localhost:8081/1',
      release: 'svc@1.4.2',
      // One envelope, flushed explicitly: no timer, no eager mid-run flush.
      flushInterval: 3_600_000,
      maxBatch: 1000,
      tags: { env: 'prod' },
      fetchImpl,
    });

    getCurrentScope().setUser({ id: 'u_123', email: 'a@b.co' });
    client.addBreadcrumb({
      type: 'navigation',
      category: 'history',
      message: 'went to /settings',
      level: 'info',
      data: { from: '/', to: '/settings' },
    });

    client.identify('u_123', { plan: 'pro' });
    client.track('checkout_completed', 'u_123', { cart_value: 42.5 });
    client.captureException(new TypeError('x is not a function'));
    client.captureMessage('payment provider returned a soft decline', 'warning');
    // NOTE the snake_case input keys: unlike the browser SDK's camelCase
    // `TransactionInput`, node takes the wire names. Passing `durationMs` here
    // type-errors, and from plain JS it would silently emit a transaction with
    // NO `duration_ms` — which is a non-`Option` `f64` on the wire, so the
    // gateway 400s the whole envelope. (Found by this fixture.)
    client.trackTransaction({
      name: 'GET /api/users',
      op: 'http',
      duration_ms: 128.4,
      status: 'ok',
      http_method: 'GET',
      http_status: 200,
      url: 'https://api.example.com/api/users',
      distinct_id: 'u_123',
      // Exercised in the fixture so the backend's `serde` deserializer sees
      // real values in these two fields, not just their absence.
      tags: { tier: 'premium' },
      extra: { request: '{"page":1}', response: '{"users":[]}' },
    });
    // A SECOND transaction with neither field set — the omit-when-empty rule is
    // the half a fixture with only the populated case cannot see, and it is the
    // half that guarantees an app not using this feature ships identical bytes.
    client.trackTransaction({ name: '/checkout', op: 'navigation', duration_ms: 42 });

    await client.flush();
    await client.close();

    expect(bodies).toHaveLength(1);
    const envelope = JSON.parse(
      bodyToString(bodies[0] as { headers: Record<string, string>; body: string | Uint8Array }),
    ) as { items: Array<Record<string, unknown>> };

    const types = envelope.items.map((i) => i.type);
    expect(types).toContain('error');
    expect(types).toContain('event');
    expect(types).toContain('identify');
    expect(types).toContain('transaction');
    expect(types.filter((t) => t === 'error')).toHaveLength(2); // exception + message

    // Every error item must carry a usable exception type: the backend's
    // `ExceptionInfo.ty` is a non-`Option` `String` with no serde default, so a
    // `null` here 400s the entire batch.
    for (const item of envelope.items) {
      if (item.type !== 'error') continue;
      const exception = item.exception as { type?: unknown; value?: unknown } | undefined | null;
      if (exception != null) {
        expect(typeof exception.type).toBe('string');
        expect(exception.type).not.toBe('');
      }
      expect(item.message ?? exception?.value).toBeTruthy();
    }

    writeWireFixture('node', envelope);
  });
});
