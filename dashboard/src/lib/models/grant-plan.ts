import type { MemberGrant, Permission, Role } from './index';
import {
  EMPTY_SELECTION,
  isEmptySelection,
  selectionToScopes,
  type ScopeRef,
  type ScopeSelection,
} from './scope-tree';

/**
 * The pure model behind EditMemberDialog.
 *
 * A member's access is exactly a map `role_id -> ScopeSelection`: the grants
 * table's only unique key is (user_id, role_id, scope_type, scope_id), so one
 * person can genuinely hold Admin on one project and Viewer on another. Each
 * entry of that map is the same (Role select + ScopeTree) pair the create
 * dialog already renders, which is what lets editing reuse the create UI
 * wholesale instead of falling back to flat per-grant dropdowns.
 *
 * Everything that decides what a tick means — which grants survive, what gets
 * written, when Save must be blocked — lives here rather than in the component,
 * where the repo's node-only vitest setup could not reach it. Nothing in this
 * file may touch Svelte or the DOM.
 */

/** One editable role block: a role, and every scope it reaches. */
export interface RoleBlock {
  /**
   * Identity for `{#each}` keying. Deliberately NOT the role id: switching a
   * block's role select would then tear the block down and rebuild it, throwing
   * away ScopeTree's internal disclosure state mid-edit.
   */
  key: string;
  roleId: string;
  selection: ScopeSelection;
  /**
   * Grants under this role whose target this dashboard cannot draw — a project
   * or app that was deleted (scope_id carries no FK, so grants outlive their
   * target) or that simply is not visible to the current admin. They are
   * preserved untouched unless explicitly dropped; without this list the diff
   * would silently revoke every grant the tree cannot render.
   */
  unmatched: MemberGrant[];
}

/** One batched POST /grants call: a role plus every scope being added to it. */
export interface GrantAddition {
  roleId: string;
  scopes: ScopeRef[];
}

export interface GrantPlan {
  additions: GrantAddition[];
  revocations: MemberGrant[];
}

export const EMPTY_PLAN: GrantPlan = Object.freeze({
  additions: Object.freeze([]) as unknown as GrantAddition[],
  revocations: Object.freeze([]) as unknown as MemberGrant[],
});

function scopeKey(scopeType: string, scopeId: string): string {
  return `${scopeType}:${scopeId}`;
}

/**
 * Seed one block per role the member holds, in first-seen order.
 *
 * `knownProjects` / `knownApps` are the ids the scope tree can actually draw;
 * anything outside them lands in `unmatched` rather than being dropped.
 */
export function grantsToBlocks(
  grants: MemberGrant[],
  orgId: string,
  knownProjects: ReadonlySet<string>,
  knownApps: ReadonlySet<string>,
): RoleBlock[] {
  const byRole = new Map<string, RoleBlock>();

  for (const g of grants) {
    let block = byRole.get(g.role_id);
    if (!block) {
      block = {
        key: `role-${byRole.size}-${g.role_id}`,
        roleId: g.role_id,
        selection: { ...EMPTY_SELECTION, projects: [], apps: [] },
        unmatched: [],
      };
      byRole.set(g.role_id, block);
    }

    if (g.scope_type === 'org') {
      // A grant pointing at a different org cannot be represented by this tree
      // (the tree only ever draws the current org), so preserve it verbatim.
      if (g.scope_id === orgId) block.selection.org = true;
      else block.unmatched.push(g);
    } else if (g.scope_type === 'project') {
      if (knownProjects.has(g.scope_id)) block.selection.projects.push(g.scope_id);
      else block.unmatched.push(g);
    } else {
      if (knownApps.has(g.scope_id)) block.selection.apps.push(g.scope_id);
      else block.unmatched.push(g);
    }
  }

  return [...byRole.values()];
}

/**
 * True when an existing grant is still reached by `targets`.
 *
 * This is a COVERAGE test, not an exact-key match, and that distinction is
 * load-bearing. `selectionToScopes` collapses a ticked app whose parent project
 * is also ticked, so a member holding both project:Billing and app:Billing/web
 * under one role seeds a selection that emits only [project:Billing]. Under
 * exact matching the app grant would fall into the revoke set, meaning merely
 * opening the dialog and pressing Save would delete a grant nobody touched.
 */
function isCovered(
  grant: MemberGrant,
  targets: ReadonlySet<string>,
  orgId: string,
  projectOfApp: Record<string, string>,
): boolean {
  if (targets.has(scopeKey('org', orgId))) return true;
  if (targets.has(scopeKey(grant.scope_type, grant.scope_id))) return true;
  if (grant.scope_type === 'app') {
    const parent = projectOfApp[grant.scope_id];
    if (parent !== undefined && targets.has(scopeKey('project', parent))) return true;
  }
  return false;
}

/**
 * Diff the edited blocks against what the member holds today.
 *
 * Grants listed in a block's `unmatched` are never revoked here — they are
 * preserved by construction, since the tree that produced `targets` could not
 * represent them in the first place.
 */
export function planGrantChanges(
  blocks: RoleBlock[],
  currentGrants: MemberGrant[],
  orgId: string,
  projectOfApp: Record<string, string>,
): GrantPlan {
  const targetsByRole = new Map<string, Set<string>>();
  for (const b of blocks) {
    const set = targetsByRole.get(b.roleId) ?? new Set<string>();
    for (const s of selectionToScopes(b.selection, orgId, projectOfApp)) {
      set.add(scopeKey(s.scope_type, s.scope_id));
    }
    targetsByRole.set(b.roleId, set);
  }

  // Grants the admin explicitly kept out of the diff by leaving them in an
  // unmatched list — keyed so the revoke sweep below can skip them.
  const preserved = new Set<string>();
  for (const b of blocks) for (const g of b.unmatched) preserved.add(g.id);

  const held = new Set<string>();
  for (const g of currentGrants) held.add(`${g.role_id}|${scopeKey(g.scope_type, g.scope_id)}`);

  const additions: GrantAddition[] = [];
  for (const [roleId, targets] of targetsByRole) {
    const scopes: ScopeRef[] = [];
    for (const key of targets) {
      if (held.has(`${roleId}|${key}`)) continue;
      const sep = key.indexOf(':');
      scopes.push({
        scope_type: key.slice(0, sep) as ScopeRef['scope_type'],
        scope_id: key.slice(sep + 1),
      });
    }
    // The API rejects an empty `scopes` with a 400, so a role with nothing new
    // simply produces no call.
    if (scopes.length) additions.push({ roleId, scopes });
  }

  const revocations: MemberGrant[] = [];
  for (const g of currentGrants) {
    if (preserved.has(g.id)) continue;
    const targets = targetsByRole.get(g.role_id);
    if (targets && isCovered(g, targets, orgId, projectOfApp)) continue;
    revocations.push(g);
  }

  return { additions, revocations };
}

export function planIsEmpty(plan: GrantPlan): boolean {
  return plan.additions.length === 0 && plan.revocations.length === 0;
}

/** How many grants the member ends up with if the whole plan succeeds. */
export function grantsAfterPlan(currentGrants: MemberGrant[], plan: GrantPlan): number {
  const added = plan.additions.reduce((n, a) => n + a.scopes.length, 0);
  return currentGrants.length - plan.revocations.length + added;
}

/**
 * True when the plan strips the member to nothing.
 *
 * Permitted, but it is the only irreversible action in the dialog — there is no
 * remove-member endpoint, so a member with zero grants vanishes from the
 * members list entirely while their account stays alive and able to sign in.
 * The caller turns the primary button red and names the consequence.
 */
export function planRemovesAllAccess(currentGrants: MemberGrant[], plan: GrantPlan): boolean {
  return currentGrants.length > 0 && grantsAfterPlan(currentGrants, plan) === 0;
}

/** Blocks with a role chosen but nothing ticked — the admin left them half-done. */
export function emptyBlockKeys(blocks: RoleBlock[]): string[] {
  return blocks.filter((b) => isEmptySelection(b.selection) && b.unmatched.length === 0).map((b) => b.key);
}

const ORG_MANAGE: Permission = 'org:manage';

function roleHasOrgManage(roleId: string, rolesById: Record<string, Role>): boolean {
  return rolesById[roleId]?.permissions.includes(ORG_MANAGE) ?? false;
}

/**
 * True when applying the plan would leave the org with no org-scoped grant
 * carrying `org:manage`.
 *
 * The backend refuses this with a 409, but only at DELETE time — by which point
 * the additions have already been written, leaving junk behind. Worse, neither
 * ordering nor PATCH can rescue the sole-owner demotion case: the replacement
 * Viewer grant carries no org:manage, so the guard trips either way. Catching
 * it here turns a mid-run failure into a disabled button.
 *
 * `orgGrants` is every grant in the org, not just this member's.
 */
export function planStripsLastOrgManage(
  orgGrants: MemberGrant[],
  plan: GrantPlan,
  rolesById: Record<string, Role>,
): boolean {
  const owning = orgGrants.filter(
    (g) => g.scope_type === 'org' && roleHasOrgManage(g.role_id, rolesById),
  );
  if (owning.length === 0) return false;

  const revoked = new Set(plan.revocations.map((g) => g.id));
  const survivors = owning.filter((g) => !revoked.has(g.id)).length;
  if (survivors > 0) return false;

  // An addition rescues it only if it grants org:manage at org scope.
  const replaces = plan.additions.some(
    (a) => roleHasOrgManage(a.roleId, rolesById) && a.scopes.some((s) => s.scope_type === 'org'),
  );
  return !replaces;
}

/**
 * Rewrite the uuid the backend names in an escalation 403 into a readable name.
 *
 * The API's message ends with `(project 7f3a…)` naming the scope it refused.
 * Degrades to the message verbatim when the pattern is absent, so a reworded
 * backend error still reaches the admin intact rather than being swallowed.
 */
export function humanizeGrantError(
  message: string,
  names: { projects: Record<string, string>; apps: Record<string, string>; orgName: string },
): string {
  return message.replace(
    /\((org|project|app) ([0-9a-fA-F-]{36})\)/g,
    (whole, kind: string, id: string) => {
      if (kind === 'org') return `(org "${names.orgName}")`;
      const name = kind === 'project' ? names.projects[id] : names.apps[id];
      return name ? `(${kind} "${name}")` : whole;
    },
  );
}
