import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DEFAULT_FRESH_MS, MAX_ENTRIES, viewCache, viewKey } from './view-cache';

// `viewCache` is a module singleton shared by every page, so each test starts
// from empty — otherwise eviction tests inherit whatever earlier tests left and
// the LRU assertions become order-dependent.
beforeEach(() => {
  viewCache.clear();
});

describe('viewKey', () => {
  it('does not collide when a separator appears inside a part', () => {
    // The whole reason parts are JSON-encoded rather than joined raw. A plain
    // join would map both of these to one key, and the collision would surface
    // as one view being served another view's payload.
    expect(viewKey('v', 'a', 'b')).not.toBe(viewKey('v', 'a|b'));
    expect(viewKey('v', 'a', 'b')).not.toBe(viewKey('v', 'a b'));
    // A part that contains the separator itself cannot forge one: JSON
    // encoding escapes the NUL inside the quoted string, so it can never
    // reach the join as a real delimiter.
    expect(viewKey('v', 'a\u0000b')).not.toBe(viewKey('v', 'a', 'b'));
  });

  it('is stable across object key order', () => {
    expect(viewKey('v', { b: 2, a: 1 })).toBe(viewKey('v', { a: 1, b: 2 }));
  });

  it('is stable across nested object key order', () => {
    expect(viewKey('v', { outer: { y: 1, x: 2 } })).toBe(viewKey('v', { outer: { x: 2, y: 1 } }));
  });

  it('keeps array order significant', () => {
    // Filter order is not meaningful to the backend, but two different filter
    // SETS must never share a key. Sorting arrays here would collapse them.
    expect(viewKey('v', ['a', 'b'])).not.toBe(viewKey('v', ['b', 'a']));
  });

  it('distinguishes an absent value from an empty string', () => {
    // `q` unset and `q=''` are different requests.
    expect(viewKey('v', undefined)).not.toBe(viewKey('v', ''));
  });

  it('distinguishes null from undefined', () => {
    // `currentEnvId` uses null for "all environments"; undefined means the
    // caller passed nothing. Collapsing them would key "all" onto "unspecified".
    expect(viewKey('v', null)).not.toBe(viewKey('v', undefined));
  });

  it('separates the view name from the parts', () => {
    expect(viewKey('issues.list', 'x')).not.toBe(viewKey('issues.listx'));
  });
});

describe('get / set', () => {
  it('returns undefined for a key never stored', () => {
    expect(viewCache.get('missing')).toBeUndefined();
  });

  it('round-trips a payload and hands back the identical reference', () => {
    const rows = [{ id: '1' }];
    expect(viewCache.set('k', rows)).toBe(rows);
    expect(viewCache.get('k')).toBe(rows);
  });

  it('overwrites an existing key rather than accumulating', () => {
    viewCache.set('k', 'first');
    viewCache.set('k', 'second');
    expect(viewCache.get('k')).toBe('second');
    expect(viewCache.size).toBe(1);
  });
});

describe('isFresh', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('is false for a missing key, so it can never read as "data exists"', () => {
    expect(viewCache.isFresh('missing')).toBe(false);
  });

  it('is true immediately after a set', () => {
    viewCache.set('k', 1);
    expect(viewCache.isFresh('k')).toBe(true);
  });

  it('is still true just inside the window', () => {
    viewCache.set('k', 1);
    vi.advanceTimersByTime(DEFAULT_FRESH_MS - 1);
    expect(viewCache.isFresh('k')).toBe(true);
  });

  it('is false once the window elapses, while the payload survives', () => {
    viewCache.set('k', 1);
    vi.advanceTimersByTime(DEFAULT_FRESH_MS);
    expect(viewCache.isFresh('k')).toBe(false);
    // The distinction the whole feature rests on: stale is not absent. The rows
    // must still be there to paint while the refresh runs behind them.
    expect(viewCache.get('k')).toBe(1);
  });

  it('honours a caller-supplied window', () => {
    viewCache.set('k', 1);
    vi.advanceTimersByTime(5_000);
    expect(viewCache.isFresh('k', 1_000)).toBe(false);
    expect(viewCache.isFresh('k', 10_000)).toBe(true);
  });
});

describe('peek', () => {
  it('exposes storedAt without counting as a use', () => {
    viewCache.set('a', 1);
    viewCache.set('b', 2);
    const entry = viewCache.peek<number>('a');
    expect(entry?.data).toBe(1);
    expect(typeof entry?.storedAt).toBe('number');

    // Overflow by EXACTLY one, so precisely one entry is evicted and which one
    // it is becomes the discriminator. `a` was peeked and `b` was not touched
    // at all, so if peek does not count as a use the order is still (a, b) and
    // `a` goes. Were peek to touch, the order would be (b, a) and `b` would go
    // instead.
    //
    // An earlier version of this test filled by MAX_ENTRIES and asserted both
    // were gone — true either way, so it passed even when `peek` was made to
    // touch. Overflow by one is what makes the assertion mean something.
    for (let i = 0; i < MAX_ENTRIES - 1; i++) viewCache.set(`fill${i}`, i);
    expect(viewCache.size).toBe(MAX_ENTRIES);
    expect(viewCache.peek('a')).toBeUndefined();
    expect(viewCache.peek('b')?.data).toBe(2);
  });
});

describe('LRU eviction', () => {
  it('never exceeds the cap', () => {
    for (let i = 0; i < MAX_ENTRIES + 50; i++) viewCache.set(`k${i}`, i);
    expect(viewCache.size).toBe(MAX_ENTRIES);
  });

  it('evicts the oldest and keeps the newest', () => {
    for (let i = 0; i < MAX_ENTRIES + 1; i++) viewCache.set(`k${i}`, i);
    expect(viewCache.get('k0')).toBeUndefined();
    expect(viewCache.get(`k${MAX_ENTRIES}`)).toBe(MAX_ENTRIES);
  });

  it('a read rescues an entry from eviction (LRU, not FIFO)', () => {
    viewCache.set('oldest', 'keep-me');
    for (let i = 0; i < MAX_ENTRIES - 1; i++) viewCache.set(`k${i}`, i);
    // Touch the oldest entry, then overflow by two. Under FIFO 'oldest' would
    // be first out; under LRU it is now the most recently used and 'k0' goes.
    expect(viewCache.get('oldest')).toBe('keep-me');
    viewCache.set('new1', 1);
    viewCache.set('new2', 2);
    expect(viewCache.get('oldest')).toBe('keep-me');
    expect(viewCache.get('k0')).toBeUndefined();
  });

  it('re-setting an existing key does not grow the map', () => {
    for (let i = 0; i < MAX_ENTRIES; i++) viewCache.set(`k${i}`, i);
    viewCache.set('k0', 'updated');
    expect(viewCache.size).toBe(MAX_ENTRIES);
    expect(viewCache.get('k0')).toBe('updated');
  });
});

describe('dedupe', () => {
  it('shares one in-flight request across concurrent callers', async () => {
    let calls = 0;
    const fetcher = () => {
      calls++;
      return new Promise<string>((resolve) => setTimeout(() => resolve('done'), 10));
    };
    const [a, b] = await Promise.all([
      viewCache.dedupe('k', fetcher),
      viewCache.dedupe('k', fetcher),
    ]);
    expect(a).toBe('done');
    expect(b).toBe('done');
    expect(calls).toBe(1);
  });

  it('does not share across different keys', async () => {
    let calls = 0;
    const fetcher = () => {
      calls++;
      return Promise.resolve('x');
    };
    await Promise.all([viewCache.dedupe('a', fetcher), viewCache.dedupe('b', fetcher)]);
    expect(calls).toBe(2);
  });

  it('starts a fresh request after the previous one settled', async () => {
    let calls = 0;
    const fetcher = () => {
      calls++;
      return Promise.resolve('x');
    };
    await viewCache.dedupe('k', fetcher);
    await viewCache.dedupe('k', fetcher);
    expect(calls).toBe(2);
  });

  it('a rejection is immediately retryable', async () => {
    // The `finally` in `dedupe` exists for this: leaving a rejected promise in
    // the map would make every later caller inherit the same failure for the
    // life of the tab.
    let calls = 0;
    const failing = () => {
      calls++;
      return Promise.reject(new Error('boom'));
    };
    await expect(viewCache.dedupe('k', failing)).rejects.toThrow('boom');
    await expect(viewCache.dedupe('k', failing)).rejects.toThrow('boom');
    expect(calls).toBe(2);
  });

  it('propagates the rejection to every concurrent caller', async () => {
    const failing = () => Promise.reject(new Error('boom'));
    const first = viewCache.dedupe('k', failing);
    const second = viewCache.dedupe('k', failing);
    await expect(first).rejects.toThrow('boom');
    await expect(second).rejects.toThrow('boom');
  });

  it('does not populate the cache on its own', async () => {
    // `dedupe` only shares the request; storing is the caller's decision, which
    // is what keeps failures out of the cache.
    await viewCache.dedupe('k', () => Promise.resolve('x'));
    expect(viewCache.get('k')).toBeUndefined();
  });
});

describe('invalidate', () => {
  it('drops only the matching prefix and reports the count', () => {
    viewCache.set('issues.list a', 1);
    viewCache.set('issues.list b', 2);
    viewCache.set('issues.detail a', 3);
    viewCache.set('events.list a', 4);
    expect(viewCache.invalidate('issues.list')).toBe(2);
    expect(viewCache.get('issues.list a')).toBeUndefined();
    expect(viewCache.get('issues.list b')).toBeUndefined();
    expect(viewCache.get('issues.detail a')).toBe(3);
    expect(viewCache.get('events.list a')).toBe(4);
  });

  it('clears a view across every scope and filter combination', () => {
    viewCache.set(viewKey('issues.list', 'app-1', 'env-1'), 1);
    viewCache.set(viewKey('issues.list', 'app-2', 'env-9'), 2);
    expect(viewCache.invalidate('issues.list')).toBe(2);
    expect(viewCache.size).toBe(0);
  });

  it('reports zero for a prefix that matches nothing', () => {
    viewCache.set('a', 1);
    expect(viewCache.invalidate('nope')).toBe(0);
    expect(viewCache.size).toBe(1);
  });
});

describe('clear', () => {
  it('drops every entry', () => {
    viewCache.set('a', 1);
    viewCache.set('b', 2);
    viewCache.clear();
    expect(viewCache.size).toBe(0);
    expect(viewCache.get('a')).toBeUndefined();
  });

  it('a request started before clear cannot populate the cache after it', async () => {
    // The logout guarantee. The promise still settles — nothing here can cancel
    // it — but it must not be handed to a caller that arrives afterwards.
    let release: (v: string) => void = () => {};
    const pending = new Promise<string>((resolve) => {
      release = resolve;
    });
    let calls = 0;
    const fetcher = () => {
      calls++;
      return pending;
    };
    const inflight = viewCache.dedupe('k', fetcher);
    viewCache.clear();
    // A post-clear caller must get its OWN request, not the pre-clear one.
    const after = viewCache.dedupe('k', () => Promise.resolve('fresh'));
    release('stale');
    await expect(inflight).resolves.toBe('stale');
    await expect(after).resolves.toBe('fresh');
    expect(calls).toBe(1);
  });
});

describe('the failed-revalidate contract', () => {
  it('leaves the previous good payload in place when set is not called', () => {
    // The caller-side rule that keeps a network blip from blanking a populated
    // table: on error, do not call `set`. Encoded here so the behaviour the
    // pages depend on is asserted somewhere.
    viewCache.set('k', ['good']);
    // ... a refresh fails, caller skips `set` ...
    expect(viewCache.get('k')).toEqual(['good']);
  });
});
