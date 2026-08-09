import { gunzipSync } from 'node:zlib';
import { afterEach, describe, expect, it } from 'vitest';
import { getClient } from '../src/client.js';
import * as Sauron from '../src/index.js';
import { writeWireFixture } from './wire-fixture-io.js';

/**
 * Captures the envelope this SDK **actually posts** and writes it to
 * `sdks/wire-fixtures/js.json`, where the backend's
 * `sauron-core --test sdk_wire_conformance` feeds it through the real
 * `serde` deserializer.
 *
 * Why this exists rather than another literal in `envelope.test.ts`: the golden
 * literals in these suites are authored here, so a shape the gateway rejects
 * outright can (and did) satisfy every assertion on both sides at once.
 * `captureMessage` shipped `exception.type: null` against a non-`Option`
 * `String` — a 400 `invalid_envelope` for the whole batch, which the transport
 * then drops without retrying. Nothing in this repo noticed.
 *
 * The body captured here is the serialized envelope off the transport, so it
 * includes `beforeSend`, sampling, scope lifting and gzip exactly as production
 * does.
 */

const originalFetch = globalThis.fetch;

interface Captured {
  headers: Record<string, string>;
  body: string;
}

function decode(c: Captured): string {
  if (c.headers['Content-Encoding'] === 'gzip') {
    return gunzipSync(c.body as unknown as Uint8Array).toString('utf8');
  }
  return c.body;
}

describe('wire fixture (js)', () => {
  afterEach(() => {
    getClient()?.teardown();
    globalThis.fetch = originalFetch;
  });

  it('posts an envelope that is captured verbatim into sdks/wire-fixtures/js.json', async () => {
    const captured: Captured[] = [];
    // Installed BEFORE init(): the client captures the native `fetch` at
    // construction time, so a later stub would never be used.
    globalThis.fetch = (async (_url: unknown, init: RequestInit) => {
      captured.push({
        headers: (init.headers ?? {}) as Record<string, string>,
        body: init.body as unknown as string,
      });
      return new Response(null, { status: 202 });
    }) as unknown as typeof fetch;

    Sauron.init({
      dsn: 'https://pk_test@localhost:8081/1',
      release: 'web@1.4.2',
      // One envelope, flushed explicitly: no timer, no eager mid-run flush.
      transport: { flushIntervalMs: 0, maxBatch: 1000 },
      tags: { env: 'prod' },
    });

    // Drive the PUBLIC api, in the order a real page would.
    Sauron.identify('u_123', { plan: 'pro' });
    Sauron.setScreen('/checkout'); // emits the reserved `$screen` event
    Sauron.track('checkout_completed', { cart_value: 42.5 });
    Sauron.captureException(new TypeError('x is not a function'));
    Sauron.captureMessage('payment provider returned a soft decline', 'warning');
    Sauron.trackTransaction({
      name: 'GET /api/users',
      op: 'http',
      durationMs: 128.4,
      status: 'ok',
      httpMethod: 'GET',
      httpStatus: 200,
      url: 'https://api.example.com/api/users',
    });

    const flushed = await getClient()!.flush(5000);
    expect(flushed).toBe(true);
    expect(captured).toHaveLength(1);

    const envelope = JSON.parse(decode(captured[0])) as {
      items: Array<Record<string, unknown>>;
    };

    // The fixture must actually exercise the item types that have broken.
    const types = envelope.items.map((i) => i.type);
    expect(types).toContain('error');
    expect(types).toContain('event');
    expect(types).toContain('identify');
    expect(types).toContain('transaction');
    expect(types.filter((t) => t === 'error')).toHaveLength(2); // exception + message

    writeWireFixture('js', envelope);
  });

  it('never emits an error item whose text is unreachable to the backend', async () => {
    // The `captureMessage` regression in one assertion: the backend's
    // `ExceptionInfo.ty` is a non-`Option` `String`, so `exception` must be
    // either absent or carry a real type string — and the message text has to
    // survive somewhere the backend reads (`message`, or `exception.value`).
    const captured: string[] = [];
    globalThis.fetch = (async (_url: unknown, init: RequestInit) => {
      captured.push(init.body as unknown as string);
      return new Response(null, { status: 202 });
    }) as unknown as typeof fetch;

    Sauron.init({
      dsn: 'https://pk_test@localhost:8081/1',
      transport: { flushIntervalMs: 0, maxBatch: 1000 },
    });
    Sauron.captureMessage('soft decline from provider', 'warning');
    await getClient()!.flush(5000);

    const item = (JSON.parse(captured[0]) as { items: Array<Record<string, unknown>> }).items[0];
    const exception = item.exception as { type?: unknown; value?: unknown } | undefined;
    if (exception !== undefined && exception !== null) {
      expect(typeof exception.type).toBe('string');
      expect(exception.type).not.toBe('');
    }
    const text = item.message ?? exception?.value;
    expect(text).toBe('soft decline from provider');
  });
});
