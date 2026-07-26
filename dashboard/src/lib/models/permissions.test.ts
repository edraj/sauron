import { describe, expect, it } from 'vitest';
import { ALL_PERMISSIONS, PERMISSION_GROUPS, PERMISSION_LABELS } from './permissions';

// Mirror of perm::ALL in backend/crates/sauron-auth/src/rbac.rs:56, in the same
// order. If the backend gains a permission and this list does not, the role
// editor silently strips it from every role on the first save — the checkbox
// grid submits its full state, and a permission with no checkbox reads as
// unchecked.
const BACKEND_CATALOG = [
  'issue:read',
  'issue:write',
  'event:read',
  'funnel:write',
  'artifact:write',
  'source:read',
  'monitor:read',
  'monitor:write',
  'app:read',
  'app:create',
  'app:update',
  'app:delete',
  'app:rotate_key',
  'project:read',
  'project:create',
  'project:update',
  'project:delete',
  'member:read',
  'member:manage',
  'role:manage',
  'org:manage',
  'alert:read',
  'alert:write',
];

describe('permission catalog', () => {
  it('matches the backend catalog exactly, in order', () => {
    expect(ALL_PERMISSIONS).toEqual(BACKEND_CATALOG);
  });

  it('has 23 permissions', () => {
    expect(ALL_PERMISSIONS).toHaveLength(23);
  });

  it('groups every permission exactly once', () => {
    const grouped = PERMISSION_GROUPS.flatMap((g) => g.permissions);
    expect([...grouped].sort()).toEqual([...ALL_PERMISSIONS].sort());
    expect(new Set(grouped).size).toBe(grouped.length);
  });

  it('labels every permission', () => {
    for (const p of ALL_PERMISSIONS) {
      expect(PERMISSION_LABELS[p], `missing label for ${p}`).toBeTruthy();
    }
  });
});
