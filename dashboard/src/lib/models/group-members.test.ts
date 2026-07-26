import { describe, expect, it } from 'vitest';
import { groupMembers, type MemberGrant } from './index';

function grant(overrides: Partial<MemberGrant>): MemberGrant {
  return {
    id: 'g1',
    user_id: 'u1',
    email: 'a@example.com',
    name: 'A',
    role_id: 'r1',
    role_name: 'Viewer',
    scope_type: 'org',
    scope_id: 'o1',
    is_active: true,
    ...overrides,
  };
}

describe('groupMembers', () => {
  it('returns one entry per user', () => {
    const out = groupMembers([
      grant({ id: 'g1', user_id: 'u1' }),
      grant({ id: 'g2', user_id: 'u1', scope_type: 'project', scope_id: 'p1' }),
      grant({ id: 'g3', user_id: 'u2', email: 'b@example.com' }),
    ]);
    expect(out).toHaveLength(2);
    expect(out[0].grants.map((g) => g.id)).toEqual(['g1', 'g2']);
    expect(out[1].grants.map((g) => g.id)).toEqual(['g3']);
  });

  it('preserves first-seen order', () => {
    const out = groupMembers([
      grant({ user_id: 'u2', email: 'b@example.com' }),
      grant({ user_id: 'u1', email: 'a@example.com' }),
    ]);
    expect(out.map((m) => m.email)).toEqual(['b@example.com', 'a@example.com']);
  });

  it('carries is_active onto the person', () => {
    const out = groupMembers([grant({ is_active: false })]);
    expect(out[0].is_active).toBe(false);
  });

  it('handles an empty list', () => {
    expect(groupMembers([])).toEqual([]);
  });
});
