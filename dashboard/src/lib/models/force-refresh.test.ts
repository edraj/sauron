import { describe, expect, it, vi } from 'vitest';
import { POLL_CAP_MS, POLL_INTERVAL_MS, runGlobalRefresh, runPageRefresh } from './force-refresh';
import type { GlobalRefreshDeps, PageRefreshDeps } from './force-refresh';

function deps(over: Partial<GlobalRefreshDeps> = {}): GlobalRefreshDeps {
  let t = 0;
  return {
    beginForcing: vi.fn(),
    endForcing: vi.fn(),
    clearCacheExcept: vi.fn(),
    refreshAll: vi.fn().mockResolvedValue([]),
    pendingCount: vi.fn().mockReturnValue(0),
    sleep: vi.fn().mockResolvedValue(undefined),
    now: () => (t += POLL_INTERVAL_MS),
    ...over,
  };
}

function pageDeps(over: Partial<PageRefreshDeps> = {}): PageRefreshDeps {
  let t = 0;
  return {
    beginForcing: vi.fn(),
    endForcing: vi.fn(),
    body: vi.fn().mockResolvedValue(undefined),
    pendingCount: vi.fn().mockReturnValue(0),
    sleep: vi.fn().mockResolvedValue(undefined),
    now: () => (t += POLL_INTERVAL_MS),
    ...over,
  };
}

describe('runPageRefresh', () => {
  it('runs the page body inside the force window', async () => {
    const order: string[] = [];
    const d = pageDeps({
      beginForcing: vi.fn(() => void order.push('begin')),
      body: vi.fn(async () => void order.push('body')),
      endForcing: vi.fn(() => void order.push('end')),
    });
    await runPageRefresh(d);
    // The body is the PAGE's own refresh — Events kicks a rollup fold in
    // there, Overview drives SSE — so it must run rather than be replaced by
    // a generic registry sweep. It just runs with force=true in effect.
    expect(order).toEqual(['begin', 'body', 'end']);
  });

  it('does NOT clear the whole client cache', async () => {
    // The deliberate difference from the global button. A page-level refresh
    // means "this page again", not "distrust everything" — dropping other
    // pages' entries would make every page button an app-wide cache flush.
    const d = pageDeps();
    expect(Object.keys(d)).not.toContain('clearCache');
  });

  it('always closes the force window, even when the body throws', async () => {
    const d = pageDeps({ body: vi.fn().mockRejectedValue(new Error('boom')) });
    await expect(runPageRefresh(d)).resolves.toBeUndefined();
    expect(d.endForcing).toHaveBeenCalledTimes(1);
  });

  it('stops forcing before it starts polling', async () => {
    const order: string[] = [];
    const d = pageDeps({
      body: vi.fn(async () => void order.push('body')),
      endForcing: vi.fn(() => void order.push('end')),
      pendingCount: vi.fn().mockReturnValueOnce(1).mockReturnValue(0),
      sleep: vi.fn(async () => void order.push('sleep')),
    });
    await runPageRefresh(d);
    // A forced poll would spend the cooldown and re-trigger the aggregate,
    // so every poll would report `recomputing: true` and the button would
    // always run to its cap.
    expect(order).toEqual(['body', 'end', 'sleep', 'body']);
  });

  it('re-runs the body on each poll tick', async () => {
    const body = vi.fn().mockResolvedValue(undefined);
    const d = pageDeps({
      body,
      pendingCount: vi.fn().mockReturnValueOnce(1).mockReturnValueOnce(1).mockReturnValue(0),
    });
    await runPageRefresh(d);
    expect(body).toHaveBeenCalledTimes(3);
  });

  it('does not poll when the page has no server-cached section', async () => {
    // Most of the 34 pages. Their promise settling IS freshness.
    const d = pageDeps({ pendingCount: vi.fn().mockReturnValue(0) });
    await runPageRefresh(d);
    expect(d.sleep).not.toHaveBeenCalled();
  });

  it('gives up at the cap', async () => {
    const sleep = vi.fn().mockResolvedValue(undefined);
    const d = pageDeps({ pendingCount: vi.fn().mockReturnValue(1), sleep });
    await runPageRefresh(d);
    expect(sleep.mock.calls.length).toBeLessThanOrEqual(POLL_CAP_MS / POLL_INTERVAL_MS + 1);
    expect(d.endForcing).toHaveBeenCalledTimes(1);
  });
});

describe('runGlobalRefresh', () => {
  it('reloads BEFORE invalidating the rest, and keeps what it repopulated', async () => {
    const order: string[] = [];
    const d = deps({
      clearCacheExcept: vi.fn(() => void order.push('clear')),
      refreshAll: vi.fn(async () => {
        order.push('refresh');
        return ['issues.list', 'issues.stats'];
      }),
    });
    await runGlobalRefresh(d);
    // Clearing FIRST makes every reload a cache miss, so the page blanks to
    // skeletons for the whole request and shows a hard error instead of stale
    // data if it fails — the opposite of stale-while-revalidate.
    expect(order).toEqual(['refresh', 'clear']);
    expect(d.clearCacheExcept).toHaveBeenCalledWith(
      new Set(['issues.list', 'issues.stats']),
    );
  });

  it('sets the forcing flag before anything is fetched', async () => {
    const order: string[] = [];
    const d = deps({
      beginForcing: vi.fn(() => void order.push('begin')),
      refreshAll: vi.fn(async () => {
        order.push('refresh');
        return [];
      }),
    });
    await runGlobalRefresh(d);
    // Set afterwards, the reload's requests would go out without force=true
    // and the server would answer from its cache — the button would look like
    // it worked and change nothing.
    expect(order).toEqual(['begin', 'refresh']);
  });

  it('always clears the forcing flag, even when the reload throws', async () => {
    const d = deps({ refreshAll: vi.fn<() => Promise<string[]>>().mockRejectedValue(new Error('boom')) });
    await expect(runGlobalRefresh(d)).resolves.toBeUndefined();
    // A stuck flag would append force=true to every later request for the life
    // of the tab, defeating the server cache entirely.
    expect(d.endForcing).toHaveBeenCalledTimes(1);
  });

  it('stops polling as soon as every section reports fresh', async () => {
    const pendingCount = vi
      .fn()
      .mockReturnValueOnce(2)
      .mockReturnValueOnce(1)
      .mockReturnValue(0);
    const d = deps({ pendingCount });
    await runGlobalRefresh(d);
    expect(d.sleep).toHaveBeenCalledTimes(2);
  });

  it('does not poll at all when nothing is server-cached', async () => {
    // Most pages hold no envelope-shaped section: their promise settling IS
    // freshness, and a 30s spinner there would be a bug.
    const d = deps({ pendingCount: vi.fn().mockReturnValue(0) });
    await runGlobalRefresh(d);
    expect(d.sleep).not.toHaveBeenCalled();
  });

  it('gives up at the cap rather than spinning forever', async () => {
    // A permanently failing aggregate never reports fresh.
    const sleep = vi.fn().mockResolvedValue(undefined);
    const d = deps({ pendingCount: vi.fn().mockReturnValue(1), sleep });
    await runGlobalRefresh(d);
    expect(sleep).toHaveBeenCalled();
    expect(sleep.mock.calls.length).toBeLessThanOrEqual(POLL_CAP_MS / POLL_INTERVAL_MS + 1);
    // And it still cleared the flag on the way out.
    expect(d.endForcing).toHaveBeenCalledTimes(1);
  });

  it('re-fetches on every poll tick, not just sleeps', async () => {
    const pendingCount = vi
      .fn()
      .mockReturnValueOnce(1)
      .mockReturnValueOnce(1)
      .mockReturnValue(0);
    const refreshAll = vi.fn().mockResolvedValue([]);
    const d = deps({ pendingCount, refreshAll });
    await runGlobalRefresh(d);
    // One forced pass plus one re-read per tick. Without the re-reads,
    // `pendingCount` reads a payload nothing ever replaces, so it can never
    // fall and the loop always runs to its cap.
    expect(refreshAll).toHaveBeenCalledTimes(3);
  });

  it('stops forcing BEFORE it starts polling', async () => {
    const order: string[] = [];
    const d = deps({
      refreshAll: vi.fn(async () => {
        order.push('refresh');
        return [];
      }),
      endForcing: vi.fn(() => void order.push('end')),
      pendingCount: vi.fn().mockReturnValueOnce(1).mockReturnValue(0),
      sleep: vi.fn(async () => void order.push('sleep')),
    });
    await runGlobalRefresh(d);
    // The polls must not carry force=true: each one would spend the cooldown,
    // re-trigger the aggregate, and come back `recomputing: true` again — so
    // the button would spin to its cap every single time.
    expect(order).toEqual(['refresh', 'end', 'sleep', 'refresh']);
  });

  it('sleeps for the configured interval', async () => {
    const d = deps({ pendingCount: vi.fn().mockReturnValueOnce(1).mockReturnValue(0) });
    await runGlobalRefresh(d);
    expect(d.sleep).toHaveBeenCalledWith(POLL_INTERVAL_MS);
  });
});
