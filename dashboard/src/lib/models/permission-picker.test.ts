import { describe, expect, it } from 'vitest';
import {
  groupState,
  inCatalogOrder,
  matchesQuery,
  receiveSelection,
} from './permission-picker';
import { PERMISSION_GROUPS } from './permissions';
import type { Permission } from './index';

describe('groupState', () => {
  const group = ['issue:read', 'issue:write'] as Permission[];

  it('is none when nothing is selected', () => {
    expect(groupState(group, new Set())).toBe('none');
  });

  it('is some when part of the group is selected', () => {
    expect(groupState(group, new Set(['issue:read'] as Permission[]))).toBe('some');
  });

  it('is all when the whole group is selected', () => {
    expect(groupState(group, new Set(group))).toBe('all');
  });
});

describe('inCatalogOrder', () => {
  it('emits in catalog order regardless of insertion order', () => {
    const catalog = PERMISSION_GROUPS.flatMap((g) => g.permissions);
    const shuffled = new Set([catalog[5], catalog[0], catalog[2]]);
    expect(inCatalogOrder(shuffled)).toEqual([catalog[0], catalog[2], catalog[5]]);
  });
});

describe('matchesQuery', () => {
  it('matches the permission string', () => {
    expect(matchesQuery('issue:read' as Permission, 'issue')).toBe(true);
  });

  it('matches the human label', () => {
    // 'org:manage' is labelled with prose that does not contain "org:manage".
    expect(matchesQuery('org:manage' as Permission, 'org')).toBe(true);
  });

  it('is true for an empty query', () => {
    expect(matchesQuery('issue:read' as Permission, '   ')).toBe(true);
  });

  it('is false for a non-match', () => {
    expect(matchesQuery('issue:read' as Permission, 'zzzznope')).toBe(false);
  });
});

// The header checkbox of a collapsed group must describe THE GROUP, never the
// search-narrowed subset, or it reports a group as fully selected while
// unselected permissions sit hidden behind the filter. This pins the real
// catalog case that makes the two diverge, so a future "simplify" that folds
// them back into one call fails here rather than in production.
describe('groupState under a search filter', () => {
  const organization = PERMISSION_GROUPS.find((g) => g.label === 'Organization')!;

  it('has a group whose search subset can diverge from the whole group', () => {
    // Guards the premise: if the catalog ever changes so that every
    // Organization permission matches "member", the divergence this whole
    // describe block exists to test would silently stop existing.
    const matches = organization.permissions.filter((p) => matchesQuery(p, 'member'));
    expect(matches.length).toBeGreaterThan(0);
    expect(matches.length).toBeLessThan(organization.permissions.length);
  });

  it('reports some for the group but all for the matching subset', () => {
    const matches = organization.permissions.filter((p) => matchesQuery(p, 'member'));
    const selected = new Set(matches);
    // What the header must render: partially selected.
    expect(groupState(organization.permissions, selected)).toBe('some');
    // What it would wrongly render if fed the filtered list instead.
    expect(groupState(matches, selected)).toBe('all');
  });
});

describe('receiveSelection', () => {
  const a = ['issue:read', 'issue:write'] as Permission[];

  it('recomputes on the very first selection', () => {
    expect(receiveSelection(null, a)).toEqual({ recompute: true, pendingEmit: null });
  });

  it("treats an identical array as this picker's own echo", () => {
    expect(receiveSelection(a, [...a])).toEqual({ recompute: false, pendingEmit: null });
  });

  it('recomputes when the contents differ', () => {
    expect(receiveSelection(a, ['issue:read'] as Permission[]).recompute).toBe(true);
  });

  it('recomputes when only the order differs', () => {
    // Emission is always in catalog order, so a differently-ordered array did
    // not come from this picker and is a genuine replace.
    expect(receiveSelection(a, ['issue:write', 'issue:read'] as Permission[]).recompute).toBe(
      true,
    );
  });

  it('consumes the baseline on an echo, not just on a replace', () => {
    expect(receiveSelection(a, [...a]).pendingEmit).toBeNull();
    expect(receiveSelection(a, ['org:manage'] as Permission[]).pendingEmit).toBeNull();
  });

  // The regression this function exists to prevent. Copying a role produces
  // one whose permissions are byte-identical to its source by construction,
  // so this sequence is a first-session-of-use path, not a corner case.
  it('recomputes for a second role whose permissions match the last emission', () => {
    // The picker emits an edit and the parent echoes it straight back...
    const afterEcho = receiveSelection(a, [...a]);
    expect(afterEcho.recompute).toBe(false);
    // ...then the dialog reopens on a DIFFERENT role that happens to carry
    // the same permission set. One emission earns exactly one echo, so this
    // must be read as a replace and reset collapse + search.
    expect(receiveSelection(afterEcho.pendingEmit, [...a]).recompute).toBe(true);
  });

  it('does not strand a stale baseline across an intervening replace', () => {
    const replaced = receiveSelection(a, ['org:manage'] as Permission[]);
    expect(receiveSelection(replaced.pendingEmit, [...a]).recompute).toBe(true);
  });
});
