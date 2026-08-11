import { describe, expect, it, vi } from 'vitest';
import { ROLE_DEFAULT_SORT, roleAccessor, type RoleMemberCounts } from './role-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { Permission, Role } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 *
 * One rule beyond that, because it is the subtler half and this file got it
 * wrong first time: **the label a row is identified by is itself a plausible
 * accessor target.** `order()` below maps rows back to `r.id`, and `id` is a
 * real `Role` field, so `name: (r) => r.id` is a mis-wiring a reader would
 * never spot. The first version of this file used ids that were the lowercased
 * names — `owner`/`Owner` — and the shared collator runs at
 * `sensitivity: 'base'`, so the two collated IDENTICALLY and the Name test
 * could not tell the right accessor from that wrong one.
 *
 * The two string-ordered columns — Name and Description — and the Name
 * fallback therefore use opaque ids deliberately ANTI-correlated with the value
 * under test: `k1 < k2 < k3` matches neither expected order. The numeric
 * columns keep readable labels, where an id accessor is both a far less
 * plausible mis-wiring and separately excluded by the direction each assertion
 * asks for; the mutation runs in the task report check all four.
 */
const PERMS: Permission[] = ['event:read', 'member:read', 'monitor:write'];

function role(over: Partial<Role> & { id: string }): Role {
  return {
    org_id: 'org-1',
    name: 'Middling',
    description: 'a role',
    is_system: false,
    permissions: PERMS.slice(0, 2),
    ...over,
  };
}

const order = (rows: Role[], key: string, dir: SortDir, counts: RoleMemberCounts = {}): string[] =>
  sortRows(rows, roleAccessor(key, counts), dir).map((r) => r.id);

describe('roleAccessor', () => {
  it('orders Name alphabetically, ignoring the system badge in the same cell', () => {
    // The server groups presets first (`is_system DESC, name ASC`). Under a
    // header labelled Name that grouping is invisible, so this column is a
    // flat A-Z — an accessor that also read `is_system` would put "Owner"
    // above "Admin" here and fail.
    //
    // Ids are opaque and anti-correlated with the names (see the file header):
    // id order is k1, k2, k3, which is neither expected order below, so
    // `name: (r) => r.id` fails here.
    const rows = [
      role({ id: 'k1', name: 'Owner', is_system: true }),
      role({ id: 'k3', name: 'Admin', is_system: false }),
      role({ id: 'k2', name: 'Viewer', is_system: true }),
    ];
    expect(order(rows, 'name', 'asc')).toEqual(['k3', 'k1', 'k2']);
    expect(order(rows, 'name', 'desc')).toEqual(['k2', 'k1', 'k3']);
  });

  it('orders Permissions by how many there are, not by which they are', () => {
    // The trap is ordering by the first permission alphabetically: `three`
    // starts with "event:read" and `one` with "monitor:write", so a
    // first-element accessor would put `three` first ascending. The count is
    // what the cell renders and what the header means.
    const rows = [
      role({ id: 'two', permissions: PERMS.slice(0, 2) }),
      role({ id: 'three', permissions: PERMS }),
      role({ id: 'one', permissions: [PERMS[2]] }),
    ];
    expect(order(rows, 'permissions', 'desc')).toEqual(['three', 'two', 'one']);
    expect(order(rows, 'permissions', 'asc')).toEqual(['one', 'two', 'three']);
  });

  it('orders Members by the injected count, treating an absent role as zero', () => {
    // A role nobody holds is missing from the map and its cell renders 0.
    // Passing the `undefined` straight through would send it to the BOTTOM of
    // both directions while displaying the smallest number in the column —
    // ordering by something other than what is on screen.
    const rows = [
      role({ id: 'few', permissions: PERMS }),
      role({ id: 'none', permissions: PERMS }),
      role({ id: 'many', permissions: PERMS }),
    ];
    const counts: RoleMemberCounts = { few: 3, many: 42 };
    expect(order(rows, 'members', 'desc', counts)).toEqual(['many', 'few', 'none']);
    expect(order(rows, 'members', 'asc', counts)).toEqual(['none', 'few', 'many']);
  });

  it('orders Members by the count, not by the permission count beside it', () => {
    // The two counts run in OPPOSITE directions, so an accessor reading
    // `permissions.length` — the neighbouring numeric column — dies here.
    const rows = [
      role({ id: 'popular', permissions: [PERMS[0]] }),
      role({ id: 'niche', permissions: PERMS }),
    ];
    expect(order(rows, 'members', 'desc', { popular: 50, niche: 1 })).toEqual([
      'popular',
      'niche',
    ]);
    expect(order(rows, 'permissions', 'desc', { popular: 50, niche: 1 })).toEqual([
      'niche',
      'popular',
    ]);
  });

  it('keeps a role with no description last in both directions', () => {
    // The trap is `?? ''`: an empty string collates BEFORE every real
    // description, so a role without one would lead the ascending list as
    // though its description were the first word in the alphabet.
    // Opaque anti-correlated ids again: Description is the other
    // string-ordered column, so `description: (r) => r.id` is the same
    // plausible mis-wiring the Name test guards against. Id order k1, k2, k3
    // is neither expected order below.
    const rows = [
      role({ id: 'k2', description: 'zzz does things' }),
      role({ id: 'k1', description: null }),
      role({ id: 'k3', description: 'aaa does things' }),
    ];
    expect(order(rows, 'description', 'asc')).toEqual(['k3', 'k2', 'k1']);
    expect(order(rows, 'description', 'desc')).toEqual(['k2', 'k3', 'k1']);
  });

  it('falls back to Name for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // "A fallback to any other column would show up here" — made true by
    // construction rather than asserted, because the earlier wording claimed
    // it and it was false for two of the three:
    //
    //  - Permissions runs opposite to name order, so it shows up. So does an
    //    id accessor: the ids run opposite to the names too.
    //  - Description and Members hold their constant defaults across these
    //    two rows — every role here has the same description and neither is
    //    in the (empty) counts map — so both TIE, and a tie collapses to
    //    input order. Supplying the rows in the OPPOSITE order to the expected
    //    one is what makes that visible; while input order was also the
    //    expected order, a fallback to either of them passed silently.
    const rows = [
      role({ id: 'k1', name: 'Zzz', permissions: [PERMS[0]] }),
      role({ id: 'k2', name: 'Aaa', permissions: PERMS }),
    ];
    expect(order(rows, 'no-such-column', 'asc')).toEqual(['k2', 'k1']);
    expect(ROLE_DEFAULT_SORT).toEqual({ key: 'name', dir: 'asc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order([role({ id: 'a' })], 'members', 'desc', { a: 1 });
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
