import { describe, expect, it } from 'vitest';
import { ADMIN_NAV } from './admin-nav';
import { PAGE_ACCESS } from './page-access';

describe('ADMIN_NAV', () => {
  // Sidebar and AdminShell both resolve visibility through PAGE_ACCESS. A nav
  // item whose href has no entry falls through resolvePageAccess's
  // fail-open branch and would show to everyone.
  it('every item has its own PAGE_ACCESS entry', () => {
    for (const item of ADMIN_NAV) {
      expect(Object.keys(PAGE_ACCESS), `${item.href} needs a PAGE_ACCESS entry`).toContain(
        item.href,
      );
    }
  });

  it('has no duplicate hrefs', () => {
    const hrefs = ADMIN_NAV.map((i) => i.href);
    expect(new Set(hrefs).size).toBe(hrefs.length);
  });
});
