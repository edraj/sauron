import { beforeEach, describe, expect, it, vi } from 'vitest';
import { refreshRegistry } from './refresh-registry';
import { setCurrentRoute } from './current-route';

let keySeq = 0;
function fake(pending = false, lastKey: string | null = `k${++keySeq}`) {
  return { reload: vi.fn().mockResolvedValue(undefined), pending, lastKey };
}

beforeEach(() => {
  refreshRegistry.reset();
  // Land on a known route so `routeVisit()` is stable within a test; each
  // `setCurrentRoute` to a NEW path bumps the visit counter.
  setCurrentRoute('/issues');
});

describe('refreshAll', () => {
  it('refreshes only views tagged with the current route', async () => {
    const onIssues = fake();
    const onOverview = fake();
    refreshRegistry.register(onIssues, '/issues');
    refreshRegistry.register(onOverview, '/overview');

    await refreshRegistry.refreshAll('/issues');

    expect(onIssues.reload).toHaveBeenCalledTimes(1);
    // Refreshing a page you are not on would pay for aggregates nobody is
    // looking at — the reach the design explicitly rules out.
    expect(onOverview.reload).not.toHaveBeenCalled();
  });

  it('re-tags a view that loads again under a new route', async () => {
    const view = fake();
    refreshRegistry.register(view, '/issues');
    refreshRegistry.register(view, '/overview');

    await refreshRegistry.refreshAll('/issues');
    expect(view.reload).not.toHaveBeenCalled();

    await refreshRegistry.refreshAll('/overview');
    expect(view.reload).toHaveBeenCalledTimes(1);
  });

  it('registers each view once, however many times it loads', async () => {
    const view = fake();
    refreshRegistry.register(view, '/issues');
    refreshRegistry.register(view, '/issues');
    refreshRegistry.register(view, '/issues');

    expect(refreshRegistry.countFor('/issues')).toBe(1);
    await refreshRegistry.refreshAll('/issues');
    // Three loads (a filter changing twice) must not mean three parallel
    // refetches of the same section.
    expect(view.reload).toHaveBeenCalledTimes(1);
  });

  it('one failing view does not stop the others', async () => {
    const bad = { reload: vi.fn().mockRejectedValue(new Error('boom')), pending: false, lastKey: 'bad' };
    const good = fake();
    refreshRegistry.register(bad, '/issues');
    refreshRegistry.register(good, '/issues');

    // `CachedView.reload` already records its own error state; the registry's
    // job is to make sure one broken section cannot leave the rest stale.
    // Resolves (does not reject) and still reports the keys it repopulated,
    // including the failing view's — its own `error` field carries the failure.
    await expect(refreshRegistry.refreshAll('/issues')).resolves.toBeInstanceOf(Array);
    expect(good.reload).toHaveBeenCalledTimes(1);
  });

  it('survives navigating away and back', async () => {
    const view = fake();
    refreshRegistry.register(view, '/issues');

    // Simulates leaving /issues and returning WITHOUT the section
    // re-registering. A clear-on-navigation design loses the entry here and
    // Refresh silently does nothing — no error, every other test still green.
    await refreshRegistry.refreshAll('/overview');
    await refreshRegistry.refreshAll('/issues');

    expect(view.reload).toHaveBeenCalledTimes(1);
  });

  it('is a no-op for a route with nothing registered', async () => {
    await expect(refreshRegistry.refreshAll('/nowhere')).resolves.toEqual([]);
  });
});

describe('visits', () => {
  it('drops the previous mount\'s views when the route is re-entered', async () => {
    const firstMount = fake();
    refreshRegistry.register(firstMount, '/issues');

    // Leave and come back. `CachedView` instances are per MOUNT, so the second
    // visit builds a whole new set and registers them.
    setCurrentRoute('/overview');
    setCurrentRoute('/issues');
    const secondMount = fake();
    refreshRegistry.register(secondMount, '/issues');

    await refreshRegistry.refreshAll('/issues');

    // Without visit scoping BOTH would fire: every request twice on the second
    // visit, three times on the third, with the orphans holding destroyed
    // component scopes alive.
    expect(secondMount.reload).toHaveBeenCalledTimes(1);
    expect(firstMount.reload).not.toHaveBeenCalled();
    expect(refreshRegistry.countFor('/issues')).toBe(1);
  });

  it('does not retain the previous mount\'s views (the leak half)', () => {
    refreshRegistry.register(fake(), '/issues');
    refreshRegistry.register(fake(), '/issues');
    expect(refreshRegistry.size()).toBe(2);

    setCurrentRoute('/overview');
    setCurrentRoute('/issues');
    refreshRegistry.register(fake(), '/issues');

    // The visit FILTER already stops old views firing, so call-count
    // assertions pass either way. Only this one fails without the eviction —
    // and what it prevents is a map that grows on every navigation, holding
    // destroyed components' retained fetchers alive for the life of the tab.
    expect(refreshRegistry.size()).toBe(1);
  });

  it('keeps every view registered within one visit', () => {
    refreshRegistry.register(fake(), '/issues');
    refreshRegistry.register(fake(), '/issues');
    refreshRegistry.register(fake(), '/issues');
    // A page with three sections must refresh all three.
    expect(refreshRegistry.countFor('/issues')).toBe(3);
  });

  it('does not bump the visit when the same path is set again', () => {
    refreshRegistry.register(fake(), '/issues');
    // Svelte re-runs the $location effect on unrelated changes; a bump there
    // would orphan the views this page already registered.
    setCurrentRoute('/issues');
    setCurrentRoute('/issues?filter=x');
    expect(refreshRegistry.countFor('/issues')).toBe(1);
  });
});

describe('pendingCount', () => {
  it('counts only pending sections, and only for the given route', () => {
    refreshRegistry.register(fake(true), '/overview');
    refreshRegistry.register(fake(true), '/overview');
    refreshRegistry.register(fake(false), '/overview');
    refreshRegistry.register(fake(true), '/issues');

    // Drives the spinner: 0 means every section on THIS page is fresh and the
    // button must stop, regardless of what other routes are still computing.
    expect(refreshRegistry.pendingCount('/overview')).toBe(2);
    expect(refreshRegistry.pendingCount('/issues')).toBe(1);
    expect(refreshRegistry.pendingCount('/members')).toBe(0);
  });

  it('reflects a view that becomes fresh', () => {
    const view = fake(true);
    refreshRegistry.register(view, '/overview');
    expect(refreshRegistry.pendingCount('/overview')).toBe(1);

    // The poll loop reads this live; a snapshot taken at registration time
    // would never reach zero and the button would always run to its cap.
    view.pending = false;
    expect(refreshRegistry.pendingCount('/overview')).toBe(0);
  });
});
