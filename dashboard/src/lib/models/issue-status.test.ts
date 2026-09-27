import { beforeEach, describe, expect, it } from 'vitest';
import { CachedView } from '../stores/cached-view.svelte';
import { viewCache } from '../stores/view-cache';
import { applyIssueStatus } from './issue-status';
import type { Issue } from './index';

beforeEach(() => {
  viewCache.clear();
});

function issue(over: Partial<Issue> = {}): Issue {
  return {
    id: 'issue-1',
    app_id: 'app1',
    fingerprint: 'f3a9',
    type: 'TypeError',
    title: 'TypeError: boom',
    culprit: null,
    level: 'error',
    status: 'unresolved',
    first_seen: '2026-09-01T00:00:00Z',
    last_seen: '2026-09-27T00:00:00Z',
    // Deliberately unlike what the write answers with below: under an
    // environment scope the record's counts are that environment's, while the
    // PATCH response carries the app-wide ones.
    times_seen: 12,
    users_seen: 3,
    assignee_id: null,
    created_at: '2026-09-01T00:00:00Z',
    updated_at: '2026-09-01T00:00:00Z',
    ...over,
  };
}

/** A write whose outcome the test controls. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function loaded(key: string, value: Issue): Promise<CachedView<Issue>> {
  const view = new CachedView<Issue>();
  await view.load(key, () => Promise.resolve(value));
  return view;
}

describe('applyIssueStatus', () => {
  // The defect this exists for. `CachedView.data` is `$state.raw` and is the
  // very object the cache holds, so `current.status = next` changed nothing on
  // screen — no signal was written — and did change the cached payload for
  // every later reader. Resolve answered 200 and the page kept saying
  // "unresolved" with a Resolve button under it.
  it('shows the new status at once, through a NEW object', async () => {
    const original = issue();
    const view = await loaded('k', original);
    const write = deferred<Issue>();

    const done = applyIssueStatus(view, 'resolved', () => write.promise);

    expect(view.data?.status).toBe('resolved');
    expect(view.data).not.toBe(original);
    expect(original.status, 'the object the cache handed out must not be edited').toBe(
      'unresolved',
    );

    write.resolve(issue({ status: 'resolved', updated_at: '2026-09-27T10:00:00Z' }));
    await done;
  });

  it('takes status and updated_at from the server, and nothing else', async () => {
    const view = await loaded('k', issue());
    await applyIssueStatus(view, 'resolved', () =>
      Promise.resolve(
        issue({ status: 'resolved', updated_at: '2026-09-27T10:00:00Z', times_seen: 9000 }),
      ),
    );
    expect(view.data?.status).toBe('resolved');
    expect(view.data?.updated_at).toBe('2026-09-27T10:00:00Z');
    expect(view.data?.times_seen, 'the scoped count on screen must survive the write').toBe(12);
  });

  it('writes the result to the cache, so coming back to the issue shows it', async () => {
    const view = await loaded('k', issue());
    await applyIssueStatus(view, 'ignored', () => Promise.resolve(issue({ status: 'ignored' })));

    const later = new CachedView<Issue>();
    let fetched = 0;
    await later.load('k', () => {
      fetched++;
      return Promise.resolve(issue());
    });
    expect(fetched, 'still inside the fresh window').toBe(0);
    expect(later.data?.status).toBe('ignored');
  });

  it('puts the ORIGINAL object back when the write fails, and rethrows', async () => {
    const original = issue();
    const view = await loaded('k', original);
    const failure = new Error('nope');

    await expect(
      applyIssueStatus(view, 'resolved', () => Promise.reject(failure)),
    ).rejects.toBe(failure);

    expect(view.data).toBe(original);
    expect(view.data?.status).toBe('unresolved');
    expect(viewCache.get<Issue>('k')).toBe(original);
  });

  // The router reuses the page across `#/issues/A` -> `#/issues/B`. A write for
  // A that lands after the reader has moved to B belongs to A's cache entry and
  // must not appear on B's page.
  it('does not write onto a different issue the page has moved to', async () => {
    const view = await loaded('issue-a', issue({ id: 'a' }));
    const write = deferred<Issue>();
    const done = applyIssueStatus(view, 'resolved', () => write.promise);

    const b = issue({ id: 'b', title: 'another issue' });
    await view.load('issue-b', () => Promise.resolve(b));

    write.resolve(issue({ id: 'a', status: 'resolved' }));
    await done;

    expect(view.data).toBe(b);
    expect(viewCache.get<Issue>('issue-a')?.status).toBe('resolved');
  });

  it('does nothing when there is nothing loaded, or nothing to change', async () => {
    let writes = 0;
    const write = () => {
      writes++;
      return Promise.resolve(issue());
    };
    expect(await applyIssueStatus(new CachedView<Issue>(), 'resolved', write)).toBe(false);

    const view = await loaded('k', issue({ status: 'resolved' }));
    expect(await applyIssueStatus(view, 'resolved', write)).toBe(false);
    expect(writes).toBe(0);
  });
});
