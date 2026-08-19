import { afterEach, describe, expect, it, vi } from 'vitest';
import { localeStore } from './locale.svelte';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe('localeStore', () => {
  it('switches locale and direction together', () => {
    localeStore.set('ar');
    expect(localeStore.locale).toBe('ar');
    expect(localeStore.rtl).toBe(true);
    expect(localeStore.tag).toBe('ar-u-nu-latn');

    localeStore.set('en');
    expect(localeStore.locale).toBe('en');
    expect(localeStore.rtl).toBe(false);
    expect(localeStore.tag).toBe('en');
  });

  /**
   * `dir` on `<html>` is what actually flips the layout, and `lang` is what
   * selects the Arabic font stack and drives screen-reader pronunciation.
   * Setting the store without reaching the DOM would leave a fully translated
   * UI rendering left-to-right in Inter — the failure this asserts against.
   *
   * There is no DOM implementation in this project's test environment, and the
   * store no-ops when `document` is undefined, so a bare assertion here would
   * throw rather than test anything. Stubbing a recorder — the house pattern
   * from `stores/nav-collapse.test.ts` — checks the exact attribute writes
   * without taking on jsdom.
   */
  it('writes lang and dir onto the document element', () => {
    const written = new Map<string, string>();
    vi.stubGlobal('document', {
      documentElement: {
        setAttribute: (name: string, value: string) => void written.set(name, value),
      },
    } as unknown as Document);

    localeStore.set('ar');
    expect(written.get('lang')).toBe('ar');
    expect(written.get('dir')).toBe('rtl');

    localeStore.set('en');
    expect(written.get('lang')).toBe('en');
    expect(written.get('dir')).toBe('ltr');
  });

  it('is idempotent', () => {
    localeStore.set('ar');
    localeStore.set('ar');
    expect(localeStore.locale).toBe('ar');
    localeStore.set('en');
  });
});

// ---------------------------------------------------------------------------
// `initialLocale()` runs once, in the constructor, at module-import time — so
// the shared singleton above cannot exercise "what does a fresh page load
// see." Proving each first-load path needs a second construction: reset the
// module registry, stub `window`, then re-import so the constructor runs
// again against the stub. Mirrors `stores/time-format.test.ts`.
// ---------------------------------------------------------------------------
function stubWindow(getItem: () => string | null, language = 'en-US'): void {
  vi.resetModules();
  vi.stubGlobal('window', {
    localStorage: { getItem, setItem: () => {} },
    navigator: { language },
  } as unknown as Window & typeof globalThis);
}

describe('initialLocale (fresh module construction)', () => {
  it('restores a stored preference', async () => {
    stubWindow(() => 'ar');
    const { localeStore: fresh } = await import('./locale.svelte');
    expect(fresh.locale).toBe('ar');
  });

  it('falls back to English when the stored value is corrupt', async () => {
    stubWindow(() => 'not-a-locale');
    const { localeStore: fresh } = await import('./locale.svelte');
    expect(fresh.locale).toBe('en');
  });

  it('honours an Arabic browser when nothing is stored', async () => {
    // `navigator.language` is a full tag, so the match is on the prefix —
    // "ar-DZ" and "ar-EG" are both Arabic.
    stubWindow(() => null, 'ar-DZ');
    const { localeStore: fresh } = await import('./locale.svelte');
    expect(fresh.locale).toBe('ar');
  });

  it('defaults to English for a non-Arabic browser', async () => {
    stubWindow(() => null, 'fr-FR');
    const { localeStore: fresh } = await import('./locale.svelte');
    expect(fresh.locale).toBe('en');
  });

  /**
   * Private-mode Safari throws from `localStorage`. A cosmetic preference
   * must not take the whole dashboard down on import, so both the read and
   * the write are guarded — this covers the read.
   */
  it('survives localStorage throwing on read', async () => {
    stubWindow(() => {
      throw new Error('SecurityError');
    }, 'ar');
    const { localeStore: fresh } = await import('./locale.svelte');
    expect(fresh.locale).toBe('ar');
  });
});
