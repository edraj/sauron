import { describe, expect, it } from 'vitest';
import { PAGE_ACCESS } from './page-access';
import { SHELL_FLAGS, resolveShell } from './shell';

describe('SHELL_FLAGS ↔ PAGE_ACCESS parity', () => {
  /**
   * Both directions, deliberately. A PAGE_ACCESS key with no shell row would
   * render that page BARE — no sidebar, no topbar — which is exactly the
   * regression the hoist exists to prevent; a shell row with no PAGE_ACCESS
   * key is dead weight that `findPageAccessKey` can never resolve to, so it
   * would sit green while covering nothing.
   */
  it('every PAGE_ACCESS key has a shell decision', () => {
    for (const key of Object.keys(PAGE_ACCESS)) {
      expect(key in SHELL_FLAGS, `SHELL_FLAGS is missing '${key}'`).toBe(true);
    }
  });

  it('every shell key is a real PAGE_ACCESS key', () => {
    for (const key of Object.keys(SHELL_FLAGS)) {
      expect(key in PAGE_ACCESS, `SHELL_FLAGS has stray key '${key}'`).toBe(true);
    }
  });
});

describe('resolveShell', () => {
  it('detail routes inherit their list page via the prefix match', () => {
    expect(resolveShell('/issues/0192aa41')).toEqual({ requireProject: true, requireApp: true });
    expect(resolveShell('/sessions/abc?x=1')).toEqual({ requireProject: true, requireApp: true });
  });

  /**
   * The flags each page passed BEFORE the hoist, spot-checked at the corners:
   * AppShell defaulted `requireProject: true` while AdminShell forwarded
   * `false`, so these are the rows a mechanical "all admin pages are alike"
   * rewrite would get wrong.
   */
  it('preserves the pre-hoist per-page flags at the defaults corners', () => {
    expect(resolveShell('/active-users')).toEqual({ requireProject: true, requireApp: false });
    expect(resolveShell('/admin/environments')).toEqual({
      requireProject: true,
      requireApp: false,
    });
    expect(resolveShell('/admin/privacy')).toEqual({ requireProject: false, requireApp: true });
    expect(resolveShell('/admin/purge')).toEqual({ requireProject: false, requireApp: true });
    expect(resolveShell('/docs')).toEqual({ requireProject: false, requireApp: false });
  });

  it('leaves the bare pages bare', () => {
    // Own layouts: wrapping them would be a visible regression, not a default.
    expect(resolveShell('/onboarding')).toBeNull();
    // No PAGE_ACCESS key at all → no shell (auth flows, legacy redirects, '*').
    expect(resolveShell('/login')).toBeNull();
    expect(resolveShell('/reset-password')).toBeNull();
    expect(resolveShell('/members')).toBeNull();
    expect(resolveShell('/no-such-page')).toBeNull();
  });
});
