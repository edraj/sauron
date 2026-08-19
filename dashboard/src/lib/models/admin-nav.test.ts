import { beforeEach, describe, expect, it } from 'vitest';
import { sessionStore } from '../stores/session.svelte';
import { ADMIN_NAV, visibleAdminNav, adminNavLocks } from './admin-nav';
import { PAGE_ACCESS, pageLockedBy } from './page-access';
import type { Permission } from './index';

const VIEWER: Permission[] = [
  'issue:read',
  'event:read',
  'monitor:read',
  'app:read',
  'env:read',
  'project:read',
  'member:read',
];

function grantOrg(permissions: Permission[]): void {
  sessionStore.access = {
    permissions: [],
    grants: [{ scope_type: 'org', scope_id: 'org-1', permissions }],
  };
}

beforeEach(() => {
  sessionStore.currentOrgId = 'org-1';
  sessionStore.currentProjectId = 'proj-1';
  sessionStore.currentAppId = 'app-1';
  sessionStore.currentEnvId = null;
});

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

// ---------------------------------------------------------------------------
// `pageLockedBy` is the nav counterpart of `lockedBy`: the same "null means you
// may act, otherwise the permission you are missing" contract that `Button`'s
// `lockedReason` already consumes, so a locked nav item and a locked button
// cannot describe the same missing grant two different ways.
// ---------------------------------------------------------------------------
describe('pageLockedBy', () => {
  it('names the missing permission for a page out of reach', () => {
    grantOrg(VIEWER);
    expect(pageLockedBy('/admin/storage')).toBe('org:manage');
    expect(pageLockedBy('/admin/alerts')).toBe('alert:read');
    expect(pageLockedBy('/admin/privacy')).toBe('pii:read');
  });

  it('is null for a page in reach', () => {
    grantOrg(VIEWER);
    expect(pageLockedBy('/admin/members')).toBe(null);
    expect(pageLockedBy('/issues')).toBe(null);
  });

  it('is null for a page that needs no permission', () => {
    grantOrg([]);
    expect(pageLockedBy('/account')).toBe(null);
    expect(pageLockedBy('/docs')).toBe(null);
    expect(pageLockedBy('/admin')).toBe(null);
  });

  // The Source Maps row now gates on the permission its LIST endpoint needs, so
  // a Viewer reaches the page and only the write controls lock.
  it('lets a Viewer reach Source Maps', () => {
    grantOrg(VIEWER);
    expect(pageLockedBy('/admin/source-maps')).toBe(null);
  });
});

// ---------------------------------------------------------------------------
// The rail and the sidebar now LOCK rather than hide, so the list they render
// is length-invariant: every member sees all twelve admin children and learns
// which grant each one needs. `visibleAdminNav` stays for AdminIndex, which
// still has to redirect to a child the member can actually open.
// ---------------------------------------------------------------------------
describe('adminNavLocks', () => {
  it('returns every admin child regardless of permissions', () => {
    grantOrg(VIEWER);
    expect(adminNavLocks()).toHaveLength(ADMIN_NAV.length);
    grantOrg([]);
    expect(adminNavLocks()).toHaveLength(ADMIN_NAV.length);
  });

  it('marks the ones out of reach with the permission they need', () => {
    grantOrg(VIEWER);
    const locks = Object.fromEntries(adminNavLocks().map((i) => [i.href, i.locked]));
    expect(locks['/admin/members']).toBe(null);
    expect(locks['/admin/source-maps']).toBe(null);
    expect(locks['/admin/storage']).toBe('org:manage');
    expect(locks['/admin/wall-of-shame']).toBe('org:manage');
  });

  it('preserves ADMIN_NAV order', () => {
    grantOrg(VIEWER);
    expect(adminNavLocks().map((i) => i.href)).toEqual(ADMIN_NAV.map((i) => i.href));
  });

  // The whole point of locking: an Admin cannot open four of the twelve pages
  // today and nothing tells them the pages exist. Now they are listed.
  it('shows an Admin the four org:manage pages they cannot open', () => {
    grantOrg([
      'member:read', 'member:manage', 'role:manage', 'project:read',
      'app:read', 'env:read', 'alert:read', 'pii:read', 'issue:read',
    ]);
    const locked = adminNavLocks()
      .filter((i) => i.locked !== null)
      .map((i) => i.href);
    expect(locked).toEqual([
      '/admin/storage',
      '/admin/wall-of-shame',
      '/admin/ingest-failures',
      '/admin/purge',
    ]);
  });
});

describe('visibleAdminNav', () => {
  it('still returns only what the member may open', () => {
    grantOrg(VIEWER);
    const hrefs = visibleAdminNav().map((i) => i.href);
    expect(hrefs).toContain('/admin/members');
    expect(hrefs).toContain('/admin/source-maps');
    expect(hrefs).not.toContain('/admin/storage');
  });
});
