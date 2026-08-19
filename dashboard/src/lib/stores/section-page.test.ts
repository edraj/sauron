import { describe, expect, it } from 'vitest';
import { SectionPage } from './section-page.svelte';
import type { ListPage } from '../models/list-state';

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

const page = <T,>(rows: T[], hasNext = false): ListPage<T> => ({ rows, hasNext });

describe('initial state', () => {
  it('holds nothing and has not loaded', () => {
    const s = new SectionPage<string>();
    expect(s.rows).toEqual([]);
    expect(s.loaded).toBe(false);
    expect(s.loading).toBe(false);
    expect(s.error).toBeNull();
    expect(s.offset).toBe(0);
    expect(s.hasNext).toBe(false);
  });

  it('is inert until load is called', () => {
    // The whole point of the card: four sections on one page must not each fire
    // an unbounded read on mount. The fetcher is a per-call argument, never
    // held, so an unloaded section has nothing it *could* call — and `loaded`
    // stays false, which is what makes the card show Fetch rather than an
    // empty list that reads as "this screen has no users".
    const s = new SectionPage<string>();
    expect(s.loaded).toBe(false);
    expect(s.loading).toBe(false);
    expect(s.rows).toEqual([]);

    let calls = 0;
    const fetcher = () => {
      calls += 1;
      return Promise.resolve(page(['a']));
    };
    expect(calls).toBe(0);
    void s.load(0, fetcher);
    expect(calls).toBe(1);
  });
});

describe('loading a page', () => {
  it('sets loading while in flight and clears it on success', async () => {
    const s = new SectionPage<string>();
    const d = deferred<ListPage<string>>();
    const p = s.load(0, () => d.promise);
    expect(s.loading).toBe(true);
    expect(s.loaded).toBe(false);
    d.resolve(page(['a', 'b'], true));
    await p;
    expect(s.loading).toBe(false);
    expect(s.loaded).toBe(true);
    expect(s.rows).toEqual(['a', 'b']);
    expect(s.hasNext).toBe(true);
    expect(s.error).toBeNull();
  });

  it('passes the requested offset to the fetcher and records it on success', async () => {
    const s = new SectionPage<string>();
    const seen: number[] = [];
    await s.load(50, (o) => {
      seen.push(o);
      return Promise.resolve(page(['x']));
    });
    expect(seen).toEqual([50]);
    expect(s.offset).toBe(50);
  });

  it('reports rows walked so far, marking more to come', async () => {
    const s = new SectionPage<string>();
    await s.load(25, () => Promise.resolve(page(['a', 'b', 'c'], true)));
    expect(s.seen).toBe(28);
    await s.load(25, () => Promise.resolve(page(['a', 'b', 'c'], false)));
    expect(s.seen).toBe(28);
    expect(s.hasNext).toBe(false);
  });
});

describe('failure', () => {
  it('records the error and leaves loaded false on a first failure', async () => {
    const s = new SectionPage<string>();
    await s.load(0, () => Promise.reject(new Error('boom')));
    expect(s.error).not.toBeNull();
    expect(s.loading).toBe(false);
    expect(s.loaded).toBe(false);
    expect(s.rows).toEqual([]);
  });

  it('clears a previous error on the next success', async () => {
    const s = new SectionPage<string>();
    await s.load(0, () => Promise.reject(new Error('boom')));
    expect(s.error).not.toBeNull();
    await s.load(0, () => Promise.resolve(page(['a'])));
    expect(s.error).toBeNull();
    expect(s.rows).toEqual(['a']);
  });

  it('keeps the rows already on screen when a later page fails', async () => {
    const s = new SectionPage<string>();
    await s.load(0, () => Promise.resolve(page(['a'], true)));
    await s.load(25, () => Promise.reject(new Error('boom')));
    // Blanking a populated card over one bad page turn is worse than showing
    // the page that is still valid.
    expect(s.rows).toEqual(['a']);
    expect(s.offset).toBe(0);
    expect(s.error).not.toBeNull();
  });
});

describe('retry targets the page that was asked for', () => {
  it('re-requests the FAILED page, not the one on screen', async () => {
    // The defect this exists for: with one offset, a failed Next leaves
    // `offset` on the page you were already reading, so Try again re-fetches
    // that page, succeeds, and the card looks recovered without having moved.
    const s = new SectionPage<string>();
    await s.load(0, () => Promise.resolve(page(['p1'], true)));
    await s.load(25, () => Promise.reject(new Error('network')));

    expect(s.offset).toBe(0); // still showing page 1
    expect(s.requestedOffset).toBe(25); // but page 2 is what was wanted

    const asked: number[] = [];
    await s.retry((o) => {
      asked.push(o);
      return Promise.resolve(page(['p2'], false));
    });

    expect(asked).toEqual([25]);
    expect(s.offset).toBe(25);
    expect(s.rows).toEqual(['p2']);
    expect(s.error).toBeNull();
  });

  it('refresh re-requests the page on screen, not the failed one', async () => {
    const s = new SectionPage<string>();
    await s.load(0, () => Promise.resolve(page(['p1'], true)));
    await s.load(25, () => Promise.reject(new Error('network')));
    const asked: number[] = [];
    await s.refresh((o) => {
      asked.push(o);
      return Promise.resolve(page(['p1-again']));
    });
    // refresh and retry mean different things after a failure, which is why
    // both exist.
    expect(asked).toEqual([0]);
  });
});

describe('overlapping requests', () => {
  it('discards a superseded response that arrives last', async () => {
    const s = new SectionPage<string>();
    const slow = deferred<ListPage<string>>();
    const fast = deferred<ListPage<string>>();

    const p1 = s.load(0, () => slow.promise);
    const p2 = s.load(25, () => fast.promise);

    fast.resolve(page(['newest'], false));
    await p2;
    expect(s.rows).toEqual(['newest']);
    expect(s.offset).toBe(25);

    // The overtaken request now returns. It must not write.
    slow.resolve(page(['stale'], true));
    await p1;
    expect(s.rows).toEqual(['newest']);
    expect(s.offset).toBe(25);
    expect(s.hasNext).toBe(false);
  });

  it('a superseded FAILURE does not clobber good state', async () => {
    const s = new SectionPage<string>();
    const slow = deferred<ListPage<string>>();
    const fast = deferred<ListPage<string>>();

    const p1 = s.load(0, () => slow.promise);
    const p2 = s.load(25, () => fast.promise);

    fast.resolve(page(['good']));
    await p2;
    slow.reject(new Error('stale failure'));
    await p1;

    expect(s.error).toBeNull();
    expect(s.rows).toEqual(['good']);
  });

  it('a superseded request does not stop the newer spinner', async () => {
    const s = new SectionPage<string>();
    const slow = deferred<ListPage<string>>();
    const fast = deferred<ListPage<string>>();

    const p1 = s.load(0, () => slow.promise);
    const p2 = s.load(25, () => fast.promise);

    // Overtaken one settles FIRST while the newer is still in flight.
    slow.resolve(page(['stale']));
    await p1;
    expect(s.loading).toBe(true);

    fast.resolve(page(['newest']));
    await p2;
    expect(s.loading).toBe(false);
    expect(s.rows).toEqual(['newest']);
  });
});
