import { describe, expect, it } from 'vitest';
import {
  emptyBlockKeys,
  grantsAfterPlan,
  grantsToBlocks,
  humanizeGrantError,
  planGrantChanges,
  planIsEmpty,
  planRemovesAllAccess,
  planStripsLastOrgManage,
  type RoleBlock,
} from './grant-plan';
import type { MemberGrant, Permission, Role } from './index';

const ORG = 'org-1';
const P_BILL = 'proj-billing';
const P_PAY = 'proj-payments';
const A_WEB = 'app-web';
const A_IOS = 'app-ios';

// app -> project, exactly as the dialog builds it from appsByProject.
const PROJECT_OF_APP: Record<string, string> = { [A_WEB]: P_BILL, [A_IOS]: P_PAY };
const KNOWN_PROJECTS = new Set([P_BILL, P_PAY]);
const KNOWN_APPS = new Set([A_WEB, A_IOS]);

let seq = 0;
function grant(over: Partial<MemberGrant> = {}): MemberGrant {
  seq += 1;
  return {
    id: `g${seq}`,
    user_id: 'u1',
    email: 'jane@acme.com',
    name: 'Jane',
    role_id: 'r-admin',
    role_name: 'Admin',
    scope_type: 'org',
    scope_id: ORG,
    is_active: true,
    ...over,
  };
}

function role(id: string, permissions: Permission[]): Role {
  return {
    id,
    org_id: ORG,
    name: id,
    description: null,
    permissions,
    is_system: false,
  } as Role;
}

/** Seed blocks the way the dialog does, then diff them unchanged. */
function roundTrip(grants: MemberGrant[]) {
  const blocks = grantsToBlocks(grants, ORG, KNOWN_PROJECTS, KNOWN_APPS);
  return { blocks, plan: planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP) };
}

describe('round trip — open the dialog, change nothing, press Save', () => {
  // This is the whole safety property of the rewrite. Every one of these must
  // produce a zero-op plan, or merely opening the dialog would mutate access.
  it('is a no-op for a single org-scoped grant', () => {
    const grants = [grant({ scope_type: 'org', scope_id: ORG })];
    expect(planIsEmpty(roundTrip(grants).plan)).toBe(true);
  });

  it('is a no-op for project and app grants under one role', () => {
    const grants = [
      grant({ scope_type: 'project', scope_id: P_PAY }),
      grant({ scope_type: 'app', scope_id: A_WEB }),
    ];
    expect(planIsEmpty(roundTrip(grants).plan)).toBe(true);
  });

  it('is a no-op when an app grant sits UNDER an already-granted project', () => {
    // The regression this design exists to prevent: selectionToScopes drops
    // app:web because its parent project is ticked, so an exact-match diff
    // would revoke the app grant with no user input at all.
    const grants = [
      grant({ scope_type: 'project', scope_id: P_BILL }),
      grant({ scope_type: 'app', scope_id: A_WEB }),
    ];
    const { plan } = roundTrip(grants);
    expect(plan.revocations).toEqual([]);
    expect(plan.additions).toEqual([]);
  });

  it('is a no-op when the org grant subsumes narrower grants under the same role', () => {
    const grants = [
      grant({ scope_type: 'org', scope_id: ORG }),
      grant({ scope_type: 'project', scope_id: P_BILL }),
      grant({ scope_type: 'app', scope_id: A_IOS }),
    ];
    expect(planIsEmpty(roundTrip(grants).plan)).toBe(true);
  });

  it('is a no-op for a member holding different roles at different scopes', () => {
    const grants = [
      grant({ role_id: 'r-admin', scope_type: 'project', scope_id: P_BILL }),
      grant({ role_id: 'r-viewer', role_name: 'Viewer', scope_type: 'project', scope_id: P_PAY }),
      grant({ role_id: 'r-viewer', role_name: 'Viewer', scope_type: 'app', scope_id: A_WEB }),
    ];
    const { blocks, plan } = roundTrip(grants);
    expect(blocks).toHaveLength(2);
    expect(planIsEmpty(plan)).toBe(true);
  });

  it('is a no-op for grants the tree cannot draw', () => {
    const grants = [
      grant({ scope_type: 'project', scope_id: 'deleted-project' }),
      grant({ scope_type: 'app', scope_id: 'invisible-app' }),
    ];
    const { blocks, plan } = roundTrip(grants);
    expect(blocks[0].unmatched).toHaveLength(2);
    expect(planIsEmpty(plan)).toBe(true);
  });
});

describe('grantsToBlocks', () => {
  it('groups by role in first-seen order and folds scopes into a selection', () => {
    const blocks = grantsToBlocks(
      [
        grant({ role_id: 'r-viewer', scope_type: 'project', scope_id: P_PAY }),
        grant({ role_id: 'r-admin', scope_type: 'org', scope_id: ORG }),
        grant({ role_id: 'r-viewer', scope_type: 'app', scope_id: A_WEB }),
      ],
      ORG,
      KNOWN_PROJECTS,
      KNOWN_APPS,
    );
    expect(blocks.map((b) => b.roleId)).toEqual(['r-viewer', 'r-admin']);
    expect(blocks[0].selection).toEqual({ org: false, projects: [P_PAY], apps: [A_WEB] });
    expect(blocks[1].selection).toEqual({ org: true, projects: [], apps: [] });
  });

  it('gives blocks a key that is not the role id, so re-roling keeps tree state', () => {
    const blocks = grantsToBlocks([grant()], ORG, KNOWN_PROJECTS, KNOWN_APPS);
    expect(blocks[0].key).not.toBe(blocks[0].roleId);
  });

  it('preserves an org grant pointing at another org as unmatched', () => {
    const blocks = grantsToBlocks(
      [grant({ scope_type: 'org', scope_id: 'other-org' })],
      ORG,
      KNOWN_PROJECTS,
      KNOWN_APPS,
    );
    expect(blocks[0].selection.org).toBe(false);
    expect(blocks[0].unmatched).toHaveLength(1);
  });

  it('returns no blocks for a member with no grants', () => {
    expect(grantsToBlocks([], ORG, KNOWN_PROJECTS, KNOWN_APPS)).toEqual([]);
  });
});

describe('planGrantChanges — additions', () => {
  it('adds a newly ticked project under an existing role', () => {
    const grants = [grant({ scope_type: 'project', scope_id: P_BILL })];
    const { blocks } = roundTrip(grants);
    blocks[0].selection.projects.push(P_PAY);
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.additions).toEqual([
      { roleId: 'r-admin', scopes: [{ scope_type: 'project', scope_id: P_PAY }] },
    ]);
    expect(plan.revocations).toEqual([]);
  });

  it('batches every new scope for one role into a single call', () => {
    const grants = [grant({ scope_type: 'project', scope_id: P_BILL })];
    const { blocks } = roundTrip(grants);
    blocks[0].selection.projects.push(P_PAY);
    blocks[0].selection.apps.push(A_IOS);
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.additions).toHaveLength(1);
    // app-ios collapses into project-payments, which now covers it.
    expect(plan.additions[0].scopes).toEqual([{ scope_type: 'project', scope_id: P_PAY }]);
  });

  it('never re-sends a scope the member already holds', () => {
    const grants = [grant({ scope_type: 'project', scope_id: P_BILL })];
    const { blocks } = roundTrip(grants);
    blocks[0].selection.projects.push(P_PAY);
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    const ids = plan.additions.flatMap((a) => a.scopes.map((s) => s.scope_id));
    expect(ids).not.toContain(P_BILL);
  });

  it('emits no call for a role whose block is empty', () => {
    const blocks: RoleBlock[] = [
      { key: 'k0', roleId: 'r-new', selection: { org: false, projects: [], apps: [] }, unmatched: [] },
    ];
    expect(planGrantChanges(blocks, [], ORG, PROJECT_OF_APP).additions).toEqual([]);
  });
});

describe('planGrantChanges — revocations', () => {
  it('revokes an unticked project', () => {
    const grants = [
      grant({ scope_type: 'project', scope_id: P_BILL }),
      grant({ scope_type: 'project', scope_id: P_PAY }),
    ];
    const { blocks } = roundTrip(grants);
    blocks[0].selection.projects = [P_BILL];
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.revocations.map((g) => g.scope_id)).toEqual([P_PAY]);
  });

  it('revokes narrower grants when the org tick is removed', () => {
    const grants = [
      grant({ scope_type: 'org', scope_id: ORG }),
      grant({ scope_type: 'project', scope_id: P_BILL }),
    ];
    const { blocks } = roundTrip(grants);
    blocks[0].selection.org = false;
    blocks[0].selection.projects = [P_BILL];
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.revocations.map((g) => g.scope_type)).toEqual(['org']);
  });

  it('turns a re-role into add-the-new then revoke-the-old', () => {
    const grants = [grant({ role_id: 'r-admin', scope_type: 'project', scope_id: P_BILL })];
    const { blocks } = roundTrip(grants);
    blocks[0].roleId = 'r-viewer';
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.additions).toEqual([
      { roleId: 'r-viewer', scopes: [{ scope_type: 'project', scope_id: P_BILL }] },
    ]);
    expect(plan.revocations.map((g) => g.role_id)).toEqual(['r-admin']);
  });

  it('revokes every grant for a role whose block was emptied', () => {
    const grants = [
      grant({ role_id: 'r-admin', scope_type: 'project', scope_id: P_BILL }),
      grant({ role_id: 'r-viewer', scope_type: 'project', scope_id: P_PAY }),
    ];
    const { blocks } = roundTrip(grants);
    blocks[0].selection.projects = [];
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.revocations.map((g) => g.role_id)).toEqual(['r-admin']);
  });

  it('revokes an unmatched grant only once it is dropped from the block', () => {
    const grants = [grant({ scope_type: 'project', scope_id: 'deleted-project' })];
    const { blocks } = roundTrip(grants);
    expect(planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP).revocations).toEqual([]);
    blocks[0].unmatched = [];
    expect(
      planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP).revocations.map((g) => g.scope_id),
    ).toEqual(['deleted-project']);
  });

  it('does not let one role’s tick rescue another role’s grant', () => {
    const grants = [
      grant({ role_id: 'r-admin', scope_type: 'project', scope_id: P_BILL }),
      grant({ role_id: 'r-viewer', scope_type: 'project', scope_id: P_BILL }),
    ];
    const { blocks } = roundTrip(grants);
    blocks[1].selection.projects = [];
    const plan = planGrantChanges(blocks, grants, ORG, PROJECT_OF_APP);
    expect(plan.revocations.map((g) => g.role_id)).toEqual(['r-viewer']);
  });
});

describe('destructive-plan guards', () => {
  it('counts the grants left after the plan', () => {
    const grants = [grant(), grant()];
    const plan = {
      additions: [{ roleId: 'r', scopes: [{ scope_type: 'project' as const, scope_id: P_PAY }] }],
      revocations: [grants[0]],
    };
    expect(grantsAfterPlan(grants, plan)).toBe(2);
  });

  it('flags a plan that removes every grant', () => {
    const grants = [grant(), grant()];
    expect(planRemovesAllAccess(grants, { additions: [], revocations: grants })).toBe(true);
  });

  it('does not flag a plan that replaces access', () => {
    const grants = [grant()];
    const plan = {
      additions: [{ roleId: 'r2', scopes: [{ scope_type: 'org' as const, scope_id: ORG }] }],
      revocations: grants,
    };
    expect(planRemovesAllAccess(grants, plan)).toBe(false);
  });

  it('does not flag a member who already had nothing', () => {
    expect(planRemovesAllAccess([], { additions: [], revocations: [] })).toBe(false);
  });

  it('lists blocks left with nothing ticked', () => {
    const blocks: RoleBlock[] = [
      { key: 'k0', roleId: 'r1', selection: { org: true, projects: [], apps: [] }, unmatched: [] },
      { key: 'k1', roleId: 'r2', selection: { org: false, projects: [], apps: [] }, unmatched: [] },
    ];
    expect(emptyBlockKeys(blocks)).toEqual(['k1']);
  });

  it('does not call a block empty when it only holds unmatched grants', () => {
    const blocks: RoleBlock[] = [
      {
        key: 'k0',
        roleId: 'r1',
        selection: { org: false, projects: [], apps: [] },
        unmatched: [grant()],
      },
    ];
    expect(emptyBlockKeys(blocks)).toEqual([]);
  });
});

describe('planStripsLastOrgManage', () => {
  const ROLES: Record<string, Role> = {
    owner: role('owner', ['org:manage', 'member:manage']),
    viewer: role('viewer', ['issue:read']),
  };

  it('blocks removing the org’s only org:manage grant', () => {
    const owner = grant({ role_id: 'owner', scope_type: 'org', scope_id: ORG });
    expect(planStripsLastOrgManage([owner], { additions: [], revocations: [owner] }, ROLES)).toBe(
      true,
    );
  });

  it('blocks demoting the sole owner to a role without org:manage', () => {
    // Neither ordering nor PATCH rescues this: the replacement carries no
    // org:manage, so the backend guard trips whichever way it is written.
    const owner = grant({ role_id: 'owner', scope_type: 'org', scope_id: ORG });
    const plan = {
      additions: [{ roleId: 'viewer', scopes: [{ scope_type: 'org' as const, scope_id: ORG }] }],
      revocations: [owner],
    };
    expect(planStripsLastOrgManage([owner], plan, ROLES)).toBe(true);
  });

  it('allows it when another owner remains', () => {
    const a = grant({ role_id: 'owner', scope_type: 'org', scope_id: ORG });
    const b = grant({ role_id: 'owner', user_id: 'u2', scope_type: 'org', scope_id: ORG });
    expect(planStripsLastOrgManage([a, b], { additions: [], revocations: [a] }, ROLES)).toBe(false);
  });

  it('allows handing org:manage to someone else in the same save', () => {
    const owner = grant({ role_id: 'owner', scope_type: 'org', scope_id: ORG });
    const plan = {
      additions: [{ roleId: 'owner', scopes: [{ scope_type: 'org' as const, scope_id: ORG }] }],
      revocations: [owner],
    };
    expect(planStripsLastOrgManage([owner], plan, ROLES)).toBe(false);
  });

  it('ignores a project-scoped org:manage grant, matching the backend guard', () => {
    const scoped = grant({ role_id: 'owner', scope_type: 'project', scope_id: P_BILL });
    expect(planStripsLastOrgManage([scoped], { additions: [], revocations: [scoped] }, ROLES)).toBe(
      false,
    );
  });
});

describe('humanizeGrantError', () => {
  const NAMES = {
    projects: { [P_BILL]: 'Billing' },
    apps: { [A_WEB]: 'Web' },
    orgName: 'Acme',
  };
  // Real uuids — the backend formats the scope as `(project <uuid>)`.
  const PU = '7f3a1c22-0000-4000-8000-000000000001';

  it('names a project the backend refused', () => {
    const out = humanizeGrantError(
      `you do not hold every permission in that role on one of the selected scopes (project ${PU})`,
      { ...NAMES, projects: { [PU]: 'Billing' } },
    );
    expect(out).toContain('(project "Billing")');
    expect(out).not.toContain(PU);
  });

  it('names the org', () => {
    expect(humanizeGrantError(`refused (org ${PU})`, NAMES)).toBe('refused (org "Acme")');
  });

  it('leaves an unknown uuid alone rather than inventing a name', () => {
    const msg = `refused (app ${PU})`;
    expect(humanizeGrantError(msg, NAMES)).toBe(msg);
  });

  it('passes a message without a scope through untouched', () => {
    expect(humanizeGrantError('you do not have access', NAMES)).toBe('you do not have access');
  });
});
