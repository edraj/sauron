import { describe, expect, it } from 'vitest';

/**
 * Every page that holds cached data must offer a way to refresh it.
 *
 * Source-scanned rather than rendered, for the same reason `page-access.test.ts`
 * and `scope.test.ts` scan source: there is no runtime registry of pages, and
 * mounting all 34 to check for a button would be slower and flakier than
 * reading them.
 *
 * This exists because the gap it closes was invisible. Thirteen pages shipped
 * with a `CachedView` and no refresh control at all — several of them showing
 * an "as of" chip that told you how stale the data was while offering no way
 * to act on it — and nothing failed.
 */
const pages = import.meta.glob('../../pages/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
});

/**
 * Matches the component USAGE, never a bare mention. Three pages carry a
 * comment saying "this page has no RefreshButton", and a `/RefreshButton/`
 * regex counted those as having one — a parity test that reported coverage it
 * had not checked.
 */
const RENDERS_BUTTON = /<RefreshButton/;

function name(path: string): string {
  return path.split('/').pop()!.replace('.svelte', '');
}

/**
 * Pages that legitimately have no refresh control.
 *
 * Keep this list honest: a page belongs here only if refreshing it is
 * meaningless, never because wiring it up was inconvenient.
 */
const NO_REFRESH_NEEDED = new Set([
  // Pre-auth and post-mail pages. They hold no cached telemetry and are
  // reached once, from a link, to perform a single action.
  'Login',
  'Register',
  'ForgotPassword',
  'ResetPassword',
  'ChangePassword',
  'ConfirmEmailChange',
  'CancelEmailChange',
  'Unsubscribe',
  'Onboarding',
  'NotFound',
  'Docs',
]);

describe('refresh parity', () => {
  it('every page holding a CachedView also renders a RefreshButton', () => {
    const missing: string[] = [];
    for (const [path, src] of Object.entries(pages)) {
      const n = name(path);
      if (NO_REFRESH_NEEDED.has(n)) continue;
      if (!/new CachedView/.test(src as string)) continue;
      if (!RENDERS_BUTTON.test(src as string)) missing.push(n);
    }
    expect(
      missing.sort(),
      `these pages cache data with no way to refresh it: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  it('every page with a RefreshButton drives it through pageRefresher', () => {
    // The unification: a hand-rolled `refreshing` flag forces only the client
    // cache, so on a server-cached page it re-requests and gets the same
    // numbers back while the button reports success.
    const rogue: string[] = [];
    for (const [path, src] of Object.entries(pages)) {
      const s = src as string;
      if (!RENDERS_BUTTON.test(s)) continue;
      if (!/pageRefresher/.test(s)) rogue.push(name(path));
    }
    expect(
      rogue.sort(),
      `these pages refresh without opening the server force window: ${rogue.join(', ')}`,
    ).toEqual([]);
  });

  it('scans a realistic number of pages (guards the glob against matching nothing)', () => {
    expect(Object.keys(pages).length).toBeGreaterThan(30);
  });
});
