import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { navCollapseStore } from './nav-collapse.svelte';

describe('navCollapseStore', () => {
  beforeEach(() => {
    // The singleton is shared across every test in this file; start each one
    // from the documented default (nothing collapsed).
    for (const label of [...navCollapseStore.collapsed]) navCollapseStore.expand(label);
  });

  it('defaults to expanded for a group it has never seen', () => {
    expect(navCollapseStore.isCollapsed('Explore')).toBe(false);
  });

  it('toggles collapsed and back', () => {
    navCollapseStore.toggle('Explore');
    expect(navCollapseStore.isCollapsed('Explore')).toBe(true);
    navCollapseStore.toggle('Explore');
    expect(navCollapseStore.isCollapsed('Explore')).toBe(false);
  });

  it('tracks groups independently', () => {
    navCollapseStore.toggle('Explore');
    expect(navCollapseStore.isCollapsed('Explore')).toBe(true);
    expect(navCollapseStore.isCollapsed('Analyze')).toBe(false);
  });

  it('expand is idempotent — the route effect calls it on every navigation', () => {
    navCollapseStore.expand('Analyze');
    navCollapseStore.expand('Analyze');
    expect(navCollapseStore.isCollapsed('Analyze')).toBe(false);
  });

  it('reassigns the set on mutation, so $state sees a new value', () => {
    // Reactivity here does not rely on the `$state` proxy intercepting `Set`
    // methods — every mutation swaps in a fresh Set. If a future edit switches
    // to `this.collapsed.add(...)`, this identity check is what fails.
    const before = navCollapseStore.collapsed;
    navCollapseStore.toggle('Explore');
    expect(navCollapseStore.collapsed).not.toBe(before);
  });

  it('persists the collapsed labels to localStorage', () => {
    const setItem = vi.fn();
    vi.stubGlobal('window', {
      localStorage: { getItem: () => null, setItem },
    } as unknown as Window & typeof globalThis);

    navCollapseStore.toggle('Uptime');

    expect(setItem).toHaveBeenCalledWith('sauron.nav.collapsed', JSON.stringify(['Uptime']));
    vi.unstubAllGlobals();
  });

  it('keeps the in-memory value when setItem throws (private-mode Safari)', () => {
    vi.stubGlobal('window', {
      localStorage: {
        getItem: () => null,
        setItem: () => {
          throw new Error('QuotaExceededError');
        },
      },
    } as unknown as Window & typeof globalThis);

    expect(() => navCollapseStore.toggle('Uptime')).not.toThrow();
    expect(navCollapseStore.isCollapsed('Uptime')).toBe(true);

    vi.unstubAllGlobals();
  });
});

// ---------------------------------------------------------------------------
// initialCollapsed() runs exactly once, in the constructor, at module-import
// time — the singleton above was already built, so none of those tests can
// exercise "what does a fresh page load see." Proving the documented fallback
// (absent, corrupt, or written by an older build all yield an empty set and
// never throw) requires forcing a second construction: reset the module
// registry, stub `window` with the stored value under test, re-import.
// ---------------------------------------------------------------------------
async function freshStoreWith(stored: string | null) {
  vi.resetModules();
  vi.stubGlobal('window', {
    localStorage: { getItem: () => stored, setItem: () => {} },
  } as unknown as Window & typeof globalThis);
  const mod = await import('./nav-collapse.svelte');
  return mod.navCollapseStore;
}

describe('initialCollapsed (fresh module construction)', () => {
  // The stub and re-imported module are local to this block — undo both so
  // nothing leaks into a test file that happens to run later in the process.
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it('restores collapsed groups from storage', async () => {
    const store = await freshStoreWith(JSON.stringify(['Explore', 'Analyze']));
    expect(store.isCollapsed('Explore')).toBe(true);
    expect(store.isCollapsed('Analyze')).toBe(true);
    expect(store.isCollapsed('Monitor')).toBe(false);
  });

  it('falls back to nothing collapsed when the stored value is not JSON', async () => {
    const store = await freshStoreWith('{not json');
    expect(store.collapsed.size).toBe(0);
  });

  it('falls back when the stored JSON is not an array', async () => {
    const store = await freshStoreWith(JSON.stringify({ Explore: true }));
    expect(store.collapsed.size).toBe(0);
  });

  it('drops non-string entries rather than trusting the array wholesale', async () => {
    const store = await freshStoreWith(JSON.stringify(['Explore', 42, null]));
    expect([...store.collapsed]).toEqual(['Explore']);
  });

  it('starts empty when nothing is stored', async () => {
    const store = await freshStoreWith(null);
    expect(store.collapsed.size).toBe(0);
  });
});
