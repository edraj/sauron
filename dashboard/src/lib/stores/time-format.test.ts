import { describe, expect, it, vi } from 'vitest';
import { timeFormatStore } from './time-format.svelte';

describe('timeFormatStore', () => {
  it('defaults to relative', () => {
    expect(timeFormatStore.mode).toBe('relative');
  });

  it('toggles to absolute and back', () => {
    timeFormatStore.toggle();
    expect(timeFormatStore.mode).toBe('absolute');
    timeFormatStore.toggle();
    expect(timeFormatStore.mode).toBe('relative');
  });

  it('set is idempotent', () => {
    timeFormatStore.set('absolute');
    timeFormatStore.set('absolute');
    expect(timeFormatStore.mode).toBe('absolute');
    timeFormatStore.set('relative');
  });
});

// ---------------------------------------------------------------------------
// initialFormat() runs exactly once, inside the constructor, at module-import
// time — the three tests above all share that one already-constructed
// singleton, so none of them can exercise "what does a fresh page load see."
// Proving the fallback documented on the store (absent, corrupt, or written
// by an older build all land on 'relative', never throw) requires forcing a
// second construction: reset the module registry, stub `window` with a
// corrupt stored value, then re-import so the constructor runs again against
// that stub.
// ---------------------------------------------------------------------------
describe('initialFormat (fresh module construction)', () => {
  it('falls back to relative when the stored value is corrupt', async () => {
    vi.resetModules();
    vi.stubGlobal('window', {
      localStorage: {
        getItem: () => 'not-a-real-mode',
        setItem: () => {},
      },
    } as unknown as Window & typeof globalThis);

    const { timeFormatStore: freshStore } = await import('./time-format.svelte');

    expect(freshStore.mode).toBe('relative');

    // This test's stub and the re-imported module are local to this one
    // test — undo both so nothing leaks into a test file that happens to
    // run after this one in the same process.
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it('restores absolute from a stored value on a fresh load', async () => {
    vi.resetModules();
    vi.stubGlobal('window', {
      localStorage: {
        getItem: () => 'absolute',
        setItem: () => {},
      },
    } as unknown as Window & typeof globalThis);

    const { timeFormatStore: freshStore } = await import('./time-format.svelte');

    expect(freshStore.mode).toBe('absolute');

    // This test's stub and the re-imported module are local to this one
    // test — undo both so nothing leaks into a test file that happens to
    // run after this one in the same process.
    vi.unstubAllGlobals();
    vi.resetModules();
  });
});
