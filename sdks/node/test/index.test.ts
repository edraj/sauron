import { describe, it, expect } from 'vitest';

import { init, track, captureMessage, flush, close, getClient } from '../src/index.js';
import type { Envelope, FetchLike } from '../src/types.js';
import { bodyToString } from './helpers.js';

function makeFakeFetch() {
  const bodies: Envelope[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    bodies.push(JSON.parse(bodyToString(init)) as Envelope);
    return { status: 200, ok: true };
  };
  return { fetchImpl, bodies };
}

describe('module facade', () => {
  it('delegates track/captureMessage to the active client and flushes', async () => {
    const fake = makeFakeFetch();
    init({ dsn: 'https://k@host/1', flushInterval: 0, fetchImpl: fake.fetchImpl });
    expect(getClient()).not.toBeNull();

    track('hello', 'user-1', { a: 1 });
    captureMessage('note');
    await flush();

    expect(fake.bodies).toHaveLength(1);
    const types = fake.bodies[0].items.map((i) => i.type).sort();
    expect(types).toEqual(['error', 'event']);

    await close();
    expect(getClient()).toBeNull();
  });

  it('no-ops when not initialized', async () => {
    // close() clears the active client in the previous test.
    track('x', 'u');
    await expect(flush()).resolves.toBeUndefined();
  });

  it('throws a typed DsnError on a clearly-invalid DSN', () => {
    expect(() => init({ dsn: 'not-a-dsn', flushInterval: 0 })).toThrow(/invalid DSN/);
  });
});
