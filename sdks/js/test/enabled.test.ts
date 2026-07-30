import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { getClient, init } from '../src/client.js';
import { startWorkflow, track } from '../src/api/product.js';
import { resetWorkflow } from '../src/workflow.js';
import { Transport } from '../src/transport/transport.js';
import { parseDsn } from '../src/dsn.js';
import type { EventItem } from '../src/types.js';

const originalFetch = globalThis.fetch;

/** A `fetch` stand-in that always answers with `status`, no matter the body. */
function fetchReturning(status: number): typeof fetch {
  return (async () => new Response(null, { status })) as unknown as typeof fetch;
}

/**
 * `isEnabled()` must reflect the transport's own permanent auto-disable on a
 * 401/403 (revoked/invalid DSN key) — not just an explicit `disable()`/`close()`
 * call. These tests drive a REAL delivery: `track()` queues an item, `flush()`
 * makes the transport actually POST it through a mocked `fetch` that answers
 * 401 (or 403), and the response runs through the transport's real
 * `classifyStatus` -> `disable` path. Nothing here calls `client.disable()` or
 * `transport.disable()` directly — if the propagation from transport to client
 * ever breaks, these are the tests that should catch it.
 */
describe('isEnabled() propagation from a live 401/403', () => {
  beforeEach(() => {
    resetWorkflow();
  });

  afterEach(() => {
    // Tear down BEFORE restoring `fetch`: `SauronClient.install()` wraps
    // whatever `fetch` was live at install time and restores exactly that
    // reference on teardown/next-init. Restoring `fetch` first would have the
    // next test's `init()` (which tears down this client as its first step)
    // clobber the next test's own mock right back to this one's.
    getClient()?.teardown();
    globalThis.fetch = originalFetch;
  });

  it('flips isEnabled() to false after the transport classifies a 401 response', async () => {
    globalThis.fetch = fetchReturning(401);
    const client = init({ dsn: 'https://pk_test@localhost:9/1' });

    expect(client.isEnabled()).toBe(true);

    track('ping'); // queues a real item on the transport's pending buffer
    const flushed = await client.flush(1000); // actually POSTs via the mocked fetch

    expect(flushed).toBe(true);
    expect(client.isEnabled()).toBe(false);

    // The workflow guards in src/api/product.ts read isEnabled() — prove the
    // propagation reaches them too, not just the raw getter.
    expect(startWorkflow('checkout')).toEqual({ status: 'disabled' });
  });

  it('flips isEnabled() to false after the transport classifies a 403 response', async () => {
    globalThis.fetch = fetchReturning(403);
    const client = init({ dsn: 'https://pk_test@localhost:9/1' });

    track('ping');
    await client.flush(1000);

    expect(client.isEnabled()).toBe(false);
  });

  it('stays enabled through an unrelated failure (e.g. a 500), unlike a 401/403', async () => {
    globalThis.fetch = fetchReturning(500);
    const client = init({ dsn: 'https://pk_test@localhost:9/1' });

    track('ping');
    // Bound the wait: a 500 retries with backoff and would otherwise hang
    // `flush()` well past a reasonable test timeout.
    await client.flush(50);

    expect(client.isEnabled()).toBe(true);
  });
});

/**
 * `SauronClient` always wires a real `onDisable` (`() => this.disable()`) that
 * itself calls `transport.disable()`, so a naive test built only against the
 * client/`init()` surface would pass even if `Transport` never learned to flip
 * its OWN `disabled` latch on 401/403 — the client's callback would silently
 * paper over the gap. This test removes that safety net: it builds a bare
 * `Transport` with an `onDisable` that does nothing at all, so the ONLY way
 * `isEnabled()` can become false is if the transport disables itself as part
 * of classifying the 401 response — i.e. it is the transport's own source of
 * truth, not something pushed into it from outside.
 */
describe('Transport auto-disables itself on 401/403 (independent of onDisable)', () => {
  function makeItem(): EventItem {
    return {
      type: 'event',
      name: 'ping',
      distinct_id: null,
      timestamp: new Date().toISOString(),
      properties: {},
    };
  }

  it('flips its own isEnabled() to false even when onDisable is a no-op', async () => {
    const dsn = parseDsn('https://pk_test@localhost:9/1');
    const fetchImpl = (async () => new Response(null, { status: 401 })) as unknown as typeof fetch;
    const transport = new Transport({
      dsn,
      options: { flushIntervalMs: 0, maxBatch: 30, maxQueueBytes: 1_000_000 },
      makeEnvelope: (items) => ({
        header: { dsn: dsn.raw, sdk: { name: 'test', version: '0' }, sent_at: new Date().toISOString(), release: null },
        context: {
          device: { device_id: 'd', family: null, model: null, arch: null },
          os: { name: null, version: null },
          app: { version: null, build: null },
          runtime: { name: null, version: null },
          user: { id: null, email: null, traits: {} },
        },
        items,
      }),
      fetchImpl,
      logger: { log: () => {}, warn: () => {} },
      onDisable: () => {
        /* deliberately does nothing — proves the transport doesn't need this
         * callback to correctly track its own disabled state */
      },
    });

    expect(transport.isEnabled()).toBe(true);
    transport.send(makeItem());
    const flushed = await transport.flush(1000);

    expect(flushed).toBe(true);
    expect(transport.isEnabled()).toBe(false);
  });
});
