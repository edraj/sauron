import type { ScopeType } from './index';

/**
 * The pure selection model behind the org/project/app scope tree.
 *
 * The dashboard has no DOM test environment, so every rule that decides what a
 * tick means — what gets sent, what renders dimmed, when submit is disabled —
 * lives here rather than inside the component, where it could not be tested.
 * Nothing in this file may touch Svelte or the DOM.
 */

/** One grant target, exactly as the members API expects it in `scopes`. */
export interface ScopeRef {
  scope_type: ScopeType;
  scope_id: string;
}

/**
 * Which nodes the admin ticked.
 *
 * A ticked project means "every app in it, now and later" — the backend stores
 * a project-level grant, so apps added afterwards are covered automatically.
 * That is why apps under a ticked project are not listed here as well.
 */
export interface ScopeSelection {
  org: boolean;
  /** Project ids ticked whole. */
  projects: string[];
  /** App ids ticked individually. */
  apps: string[];
}

// One shared instance is safe because it is frozen and can never diverge.
const NO_IDS: string[] = [];
Object.freeze(NO_IDS);

/**
 * Frozen so a caller that assigns it directly and then pushes gets a loud
 * TypeError instead of silently poisoning the shared default for every later
 * form. Copy it (`{ ...EMPTY_SELECTION, projects: [] }`) before mutating.
 */
export const EMPTY_SELECTION: ScopeSelection = Object.freeze({
  org: false,
  projects: NO_IDS,
  apps: NO_IDS,
});

/**
 * Collapse a selection to the minimum set of grants that covers it.
 *
 * A ticked org supersedes everything, and a ticked project supersedes its own
 * apps. Resolving the second case needs the parent of each app, which the
 * selection alone does not carry — hence the optional `projectOfApp` map from
 * app id to project id. Pass it whenever the tree is known (the picker always
 * knows it); without it every ticked app is emitted, which is redundant but
 * never wrong, since the backend de-duplicates and a narrower grant beside a
 * wider one grants nothing extra.
 *
 * Emission order is stable: org, then projects in the given order, then apps.
 */
export function selectionToScopes(
  sel: ScopeSelection,
  orgId: string,
  projectOfApp?: Record<string, string>,
): ScopeRef[] {
  if (sel.org) {
    return [{ scope_type: 'org', scope_id: orgId }];
  }

  const tickedProjects = new Set(sel.projects);
  const scopes: ScopeRef[] = sel.projects.map((id) => ({
    scope_type: 'project',
    scope_id: id,
  }));

  for (const appId of sel.apps) {
    const parent = projectOfApp?.[appId];
    if (parent !== undefined && tickedProjects.has(parent)) continue;
    scopes.push({ scope_type: 'app', scope_id: appId });
  }

  return scopes;
}

export type CheckState = 'checked' | 'indeterminate' | 'unchecked';

/**
 * Tri-state for a project row, given the app ids that live under it.
 *
 * A project with no ticked apps and no tick of its own is 'unchecked', so a
 * project with zero apps can never read as partially selected.
 */
export function projectCheckState(
  sel: ScopeSelection,
  projectId: string,
  appIds: string[],
): CheckState {
  if (sel.projects.includes(projectId)) return 'checked';
  if (appIds.some((id) => sel.apps.includes(id))) return 'indeterminate';
  return 'unchecked';
}

/** True when nothing is selected — submit must be disabled, the API rejects an
 * empty `scopes` with a 400. */
export function isEmptySelection(sel: ScopeSelection): boolean {
  return !sel.org && sel.projects.length === 0 && sel.apps.length === 0;
}

/**
 * The one-line English summary of a selection ("Full access to Acme",
 * "2 projects, 1 app").
 *
 * Lives here rather than inline in ScopeTree so it is testable at all — the
 * dashboard has no DOM test environment, so a component-local `$derived` could
 * never be exercised. Any future surface that has to phrase a selection the
 * same way the picker does should call this rather than restate the rules.
 */
export function describeSelection(
  sel: ScopeSelection,
  orgId: string,
  orgName: string,
  projectOfApp?: Record<string, string>,
): string {
  if (isEmptySelection(sel)) return 'Nothing selected yet.';
  if (sel.org) return `Full access to ${orgName}`;
  const scopes = selectionToScopes(sel, orgId, projectOfApp);
  const projectCount = scopes.filter((s) => s.scope_type === 'project').length;
  const appCount = scopes.length - projectCount;
  const parts: string[] = [];
  if (projectCount) parts.push(`${projectCount} project${projectCount === 1 ? '' : 's'}`);
  if (appCount) parts.push(`${appCount} app${appCount === 1 ? '' : 's'}`);
  return parts.join(', ');
}

/**
 * True when this row is already covered by an ancestor tick and must render
 * dimmed and disabled — ticking it would add a grant that changes nothing.
 *
 * For `level: 'project'` the `projectId` is the row's own id; for `'app'` it is
 * the id of the project the app sits under.
 */
export function isImpliedByAncestor(
  sel: ScopeSelection,
  level: 'project' | 'app',
  projectId: string,
): boolean {
  if (sel.org) return true;
  return level === 'app' && sel.projects.includes(projectId);
}
