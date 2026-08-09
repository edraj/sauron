import { beforeEach, describe, expect, it, vi } from 'vitest';
import { CachedView } from './cached-view.svelte';
import { viewCache } from './view-cache';

beforeEach(() => {
  viewCache.clear();
});

/** Resolves on the next macrotask, after pending promise chains have run. */
const settle = () => new Promise((r) => setTimeout(r, 0));

/** A fetcher whose resolution the test controls. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('cold load', () => {
  it('starts in the loading state with no data', () => {
    const v = new CachedView<string[]>();
    expect(v.loading).toBe(true);
    expect(v.revalidating).toBe(false);
    expect(v.data).toBeUndefined();
    expect(v.hasData).toBe(false);
  });

  it('shows a skeleton, not a revalidate, when nothing is cached', async () => {
    const v = new CachedView<string[]>();
    const d = deferred<string[]>();
    const p = v.load('k', () => d.promise);
    // The distinction the whole feature rests on: cold load is `loading`, never
    // `revalidating`, because there is nothing on screen to revalidate.
    expect(v.loading).toBe(true);
    expect(v.revalidating).toBe(false);
    d.resolve(['a']);
    await p;
    expect(v.loading).toBe(false);
    expect(v.data).toEqual(['a']);
    expect(v.error).toBeNull();
  });

  it('populates the shared cache so another view can hit it', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.resolve(['a']));
    const other = new CachedView<string[]>();
    let calls = 0;
    await other.load('k', () => {
      calls++;
      return Promise.resolve(['b']);
    });
    expect(calls).toBe(0);
    expect(other.data).toEqual(['a']);
    expect(other.loading).toBe(false);
  });
});

describe('switching to an uncached key', () => {
  it('goes back to loading and drops the previous payload', async () => {
    const v = new CachedView<string[]>();
    await v.load('env-a', () => Promise.resolve(['rows-for-env-a']));
    expect(v.loading).toBe(false);

    const d = deferred<string[]>();
    const p = v.load('env-b', () => d.promise);
    // Synchronously after switching: nothing from env-a may still be readable.
    // A template that renders `data` next to a spinner rather than instead of it
    // would otherwise label env-a's rows as env-b's.
    expect(v.loading).toBe(true);
    expect(v.data).toBeUndefined();
    expect(v.revalidating).toBe(false);

    d.resolve(['rows-for-env-b']);
    await p;
    expect(v.data).toEqual(['rows-for-env-b']);
  });

  it('clears a stale error when switching keys', async () => {
    const v = new CachedView<string[]>();
    await v.load('env-a', () => Promise.reject(new Error('boom')));
    expect(v.error).toBeTruthy();
    const d = deferred<string[]>();
    const p = v.load('env-b', () => d.promise);
    expect(v.error).toBeNull();
    d.resolve(['ok']);
    await p;
  });
});

describe('falsy payloads are cache hits', () => {
  // `undefined` is the cache's only "absent" marker, so a view whose payload is
  // legitimately null / 0 / '' must still paint from cache instead of showing a
  // skeleton. A truthiness check here would regress exactly these.
  it('treats a null payload as present', async () => {
    const v = new CachedView<string | null>();
    await v.load('k', () => Promise.resolve(null));
    expect(v.data).toBeNull();
    expect(v.loading).toBe(false);

    const second = new CachedView<string | null>();
    let calls = 0;
    await second.load('k', () => {
      calls++;
      return Promise.resolve('should not be fetched');
    });
    expect(calls).toBe(0);
    expect(second.loading).toBe(false);
    expect(second.data).toBeNull();
  });

  it('treats 0 and the empty string as present', async () => {
    const zero = new CachedView<number>();
    await zero.load('z', () => Promise.resolve(0));
    const zeroAgain = new CachedView<number>();
    await zeroAgain.load('z', () => Promise.reject(new Error('must not be called')));
    expect(zeroAgain.data).toBe(0);
    expect(zeroAgain.loading).toBe(false);
    expect(zeroAgain.error).toBeNull();

    const empty = new CachedView<string>();
    await empty.load('e', () => Promise.resolve(''));
    const emptyAgain = new CachedView<string>();
    await emptyAgain.load('e', () => Promise.reject(new Error('must not be called')));
    expect(emptyAgain.data).toBe('');
    expect(emptyAgain.loading).toBe(false);
  });
});

describe('warm load', () => {
  it('paints cached data synchronously and never flips loading on', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.resolve(['a']));

    const second = new CachedView<string[]>();
    const d = deferred<string[]>();
    // Force so it revalidates rather than short-circuiting on freshness.
    const p = second.load('k', () => d.promise, true);
    // Synchronously after the call: data is already there and loading never went
    // true. This is what stops the navigation flash.
    expect(second.loading).toBe(false);
    expect(second.data).toEqual(['a']);
    expect(second.revalidating).toBe(true);
    d.resolve(['fresh']);
    await p;
    expect(second.data).toEqual(['fresh']);
    expect(second.revalidating).toBe(false);
  });

  it('skips the network entirely inside the fresh window', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.resolve(['a']));
    let calls = 0;
    await v.load('k', () => {
      calls++;
      return Promise.resolve(['b']);
    });
    expect(calls).toBe(0);
    expect(v.data).toEqual(['a']);
    expect(v.revalidating).toBe(false);
  });

  it('force reaches the network even when fresh', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.resolve(['a']));
    let calls = 0;
    await v.load(
      'k',
      () => {
        calls++;
        return Promise.resolve(['b']);
      },
      true,
    );
    expect(calls).toBe(1);
    expect(v.data).toEqual(['b']);
  });

  it('revalidates once the fresh window has passed, without forcing', async () => {
    vi.useFakeTimers();
    try {
      const v = new CachedView<string[]>();
      await v.load('k', () => Promise.resolve(['a']));
      vi.advanceTimersByTime(60_001);
      let calls = 0;
      const p = v.load('k', () => {
        calls++;
        return Promise.resolve(['b']);
      });
      expect(v.revalidating).toBe(true);
      expect(v.data).toEqual(['a']);
      await p;
      expect(calls).toBe(1);
      expect(v.data).toEqual(['b']);
    } finally {
      vi.useRealTimers();
    }
  });

  it('honours a custom fresh window', async () => {
    vi.useFakeTimers();
    try {
      const v = new CachedView<string[]>(1_000);
      await v.load('k', () => Promise.resolve(['a']));
      vi.advanceTimersByTime(1_001);
      let calls = 0;
      await v.load('k', () => {
        calls++;
        return Promise.resolve(['b']);
      });
      expect(calls).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('failure handling', () => {
  it('a cold failure surfaces an error and no data', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(new Error('boom')));
    expect(v.error).toBeTruthy();
    expect(v.data).toBeUndefined();
    expect(v.loading).toBe(false);
  });

  // The two halves of "a failed refresh over good data". These were ONE test
  // that passed `force = true` while asserting the background contract, so it
  // asserted that an explicit Refresh fails silently — the exact bug reviewers
  // then found on eight pages. Splitting them is what makes the distinction
  // testable at all.

  it('a failed BACKGROUND revalidate keeps the data and stays quiet', async () => {
    // Nobody asked for this fetch. The screen is still truthful, so blanking a
    // populated table or shouting about a blip would both be worse than silence.
    // freshMs = 0 so the entry is never inside the fresh window and a
    // non-forced load genuinely goes to the network.
    const v = new CachedView<string[]>(0);
    await v.load('k', () => Promise.resolve(['good']));
    await v.load('k', () => Promise.reject(new Error('boom')));
    expect(v.data).toEqual(['good']);
    expect(v.error).toBeNull();
    expect(v.revalidating).toBe(false);
    expect(v.loading).toBe(false);
  });

  it('a failed EXPLICIT refresh keeps the data but reports the failure', async () => {
    // The user asked for current data and did not get it. Staying silent leaves
    // stale rows presented as fresh with a spinner that merely stops, which
    // reads as success.
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.resolve(['good']));
    await v.load('k', () => Promise.reject(new Error('boom')), true);
    expect(v.data, 'stale data is better than a blank table').toEqual(['good']);
    expect(v.error).toBeTruthy();
    expect(v.revalidating).toBe(false);
    expect(v.loading).toBe(false);
  });

  it('a forced fetch refuses to join a flight that started before it', async () => {
    // A re-list after a delete must not attach to a GET issued before the
    // delete: that response describes the pre-delete world and `set` would
    // cache it, putting the deleted row back for the whole fresh window.
    const v1 = new CachedView<string[]>();
    const v2 = new CachedView<string[]>();
    let resolveFirst: (v: string[]) => void = () => {};
    const first = new Promise<string[]>((r) => {
      resolveFirst = r;
    });
    let secondCalls = 0;

    const p1 = v1.load('k', () => first);
    const p2 = v2.load(
      'k',
      () => {
        secondCalls++;
        return Promise.resolve(['after']);
      },
      true,
    );
    resolveFirst(['before']);
    await Promise.all([p1, p2]);

    expect(secondCalls, 'the forced fetch issued its own request').toBe(1);
    expect(v2.data).toEqual(['after']);
  });

  it('idle() settles a page whose inputs do not exist yet', async () => {
    // `loading` starts true and only a completed load clears it, so a page that
    // renders before an app is picked would spin forever on a request never made.
    const v = new CachedView<string[]>();
    expect(v.loading).toBe(true);
    v.idle();
    expect(v.loading).toBe(false);
    expect(v.data).toBeUndefined();
    expect(v.error).toBeNull();
  });

  it('idle() abandons an in-flight load rather than letting it land', async () => {
    const v = new CachedView<string[]>();
    let resolve: (v: string[]) => void = () => {};
    const p = v.load('k', () => new Promise<string[]>((r) => (resolve = r)));
    v.idle();
    resolve(['late']);
    await p;
    expect(v.data, 'a response for cleared inputs must not repopulate the page').toBeUndefined();
    expect(v.loading).toBe(false);
  });

  it('does not cache the failure, so the next attempt retries', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(new Error('boom')));
    let calls = 0;
    await v.load('k', () => {
      calls++;
      return Promise.resolve(['ok']);
    });
    expect(calls).toBe(1);
    expect(v.data).toEqual(['ok']);
    expect(v.error).toBeNull();
  });

  it('clears a previous error once a later load succeeds', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(new Error('boom')));
    expect(v.error).toBeTruthy();
    await v.load('k', () => Promise.resolve(['ok']), true);
    expect(v.error).toBeNull();
  });
});

describe('out-of-order responses', () => {
  it('a slow earlier load cannot overwrite a newer one', async () => {
    const v = new CachedView<string[]>();
    const slow = deferred<string[]>();
    const fast = deferred<string[]>();

    const first = v.load('key-old', () => slow.promise);
    const second = v.load('key-new', () => fast.promise);

    fast.resolve(['new']);
    await second;
    expect(v.data).toEqual(['new']);

    // The stale response lands last. Without the generation guard it would win
    // purely because it arrived later.
    slow.resolve(['old']);
    await first;
    await settle();
    expect(v.data).toEqual(['new']);
  });

  it('a slow earlier FAILURE cannot clear a newer success', async () => {
    const v = new CachedView<string[]>();
    const slow = deferred<string[]>();
    const fast = deferred<string[]>();

    const first = v.load('key-old', () => slow.promise);
    const second = v.load('key-new', () => fast.promise);

    fast.resolve(['new']);
    await second;

    slow.reject(new Error('stale failure'));
    await first;
    await settle();
    expect(v.data).toEqual(['new']);
    expect(v.error).toBeNull();
  });

  it('a stale response does not clear the newer load flags', async () => {
    const v = new CachedView<string[]>();
    const slow = deferred<string[]>();
    const pending = deferred<string[]>();

    const first = v.load('key-old', () => slow.promise);
    const second = v.load('key-new', () => pending.promise);

    // Resolve the OLD one while the new one is still in flight. Its `finally`
    // must not clear `loading`, or the page would render an empty table as
    // though the newer load had finished.
    slow.resolve(['old']);
    await first;
    await settle();
    expect(v.loading).toBe(true);
    expect(v.data).toBeUndefined();

    pending.resolve(['new']);
    await second;
    expect(v.loading).toBe(false);
    expect(v.data).toEqual(['new']);
  });
});

describe('reset', () => {
  it('returns to the pre-load state', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.resolve(['a']));
    v.reset();
    expect(v.data).toBeUndefined();
    expect(v.loading).toBe(true);
    expect(v.error).toBeNull();
    expect(v.hasData).toBe(false);
  });

  it('an in-flight load cannot land after a reset', async () => {
    const v = new CachedView<string[]>();
    const d = deferred<string[]>();
    const p = v.load('k', () => d.promise);
    v.reset();
    d.resolve(['late']);
    await p;
    await settle();
    expect(v.data).toBeUndefined();
    expect(v.loading).toBe(true);
  });
});

describe('data is not deep-proxied', () => {
  it('hands back the exact cached reference', async () => {
    // `$state.raw` is load-bearing: a deep proxy here would let a page write
    // through `view.data` into the shared cached object.
    const rows = [{ id: '1' }];
    const v = new CachedView<{ id: string }[]>();
    await v.load('k', () => Promise.resolve(rows));
    expect(v.data).toBe(rows);
    expect(viewCache.get('k')).toBe(rows);
  });
});

describe('errorStatus', () => {
  /** An axios-shaped rejection as `normalizeError` would have produced it. */
  const normalized = (status: number, message: string) => ({
    status,
    code: status === 403 ? 'forbidden' : 'http_error',
    message,
    isNetwork: false,
  });

  it('carries the HTTP status alongside the message', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(normalized(403, 'filtering by tag requires event:read')));
    expect(v.error).toBe('filtering by tag requires event:read');
    // The point of the field: a page can distinguish "permanent, stop offering
    // Retry" from "transient" without matching on the prose.
    expect(v.errorStatus).toBe(403);
  });

  it('reports null for a network failure rather than 0', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject({ status: 0, code: 'network_error', message: 'down', isNetwork: true }));
    expect(v.error).toBe('down');
    // `normalizeError` uses 0 as its "never reached the server" sentinel. Leaking
    // that outward would make every consumer learn the sentinel; `errorStatus`
    // means "the server answered with this" and nothing else.
    expect(v.errorStatus).toBeNull();
  });

  it('reports null for a plain Error, which has no status at all', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(new Error('boom')));
    expect(v.error).toBe('boom');
    expect(v.errorStatus).toBeNull();
  });

  it('clears the status when a later load succeeds', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(normalized(403, 'nope')));
    expect(v.errorStatus).toBe(403);
    await v.load('k', async () => ['a'], true);
    // Both halves of the fact must clear together. A stale 403 left behind here
    // would keep a page rendering "this filter needs more access" over data that
    // loaded fine.
    expect(v.error).toBeNull();
    expect(v.errorStatus).toBeNull();
  });

  it('is set on a failed EXPLICIT refresh over good data, next to the kept rows', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', async () => ['a']);
    await v.load('k', () => Promise.reject(normalized(403, 'nope')), true);
    expect(v.data).toEqual(['a']);
    expect(v.errorStatus).toBe(403);
  });

  it('reset() and idle() clear it', async () => {
    const v = new CachedView<string[]>();
    await v.load('k', () => Promise.reject(normalized(403, 'nope')));
    v.reset();
    expect(v.errorStatus).toBeNull();
    await v.load('k2', () => Promise.reject(normalized(403, 'nope')));
    v.idle();
    expect(v.errorStatus).toBeNull();
  });
});
