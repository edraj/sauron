import { describe, expect, it } from 'vitest';
import {
  EMPTY_SELECTION,
  isEmptySelection,
  isImpliedByAncestor,
  projectCheckState,
  selectionToScopes,
  type ScopeSelection,
} from './scope-tree';

const ORG_ID = 'o1';

function selection(overrides: Partial<ScopeSelection> = {}): ScopeSelection {
  return { org: false, projects: [], apps: [], envs: [], ...overrides };
}

describe('EMPTY_SELECTION', () => {
  it('selects nothing', () => {
    expect(EMPTY_SELECTION).toEqual({ org: false, projects: [], apps: [], envs: [] });
    expect(isEmptySelection(EMPTY_SELECTION)).toBe(true);
    expect(selectionToScopes(EMPTY_SELECTION, ORG_ID)).toEqual([]);
  });

  it('is frozen so a caller cannot poison the shared default', () => {
    expect(() => EMPTY_SELECTION.projects.push('p1')).toThrow();
    expect(EMPTY_SELECTION.projects).toEqual([]);
  });
});

describe('selectionToScopes', () => {
  it('returns nothing for an empty selection', () => {
    expect(selectionToScopes(selection(), ORG_ID)).toEqual([]);
  });

  it('collapses to the org alone when the org is ticked', () => {
    const out = selectionToScopes(
      selection({ org: true, projects: ['p1'], apps: ['a1'] }),
      ORG_ID,
    );
    expect(out).toEqual([{ scope_type: 'org', scope_id: ORG_ID }]);
  });

  it('emits one project scope per ticked project', () => {
    expect(selectionToScopes(selection({ projects: ['p1'] }), ORG_ID)).toEqual([
      { scope_type: 'project', scope_id: 'p1' },
    ]);
  });

  it('emits one app scope per ticked app, in the given order', () => {
    expect(selectionToScopes(selection({ apps: ['a2', 'a1'] }), ORG_ID)).toEqual([
      { scope_type: 'app', scope_id: 'a2' },
      { scope_type: 'app', scope_id: 'a1' },
    ]);
  });

  it('drops apps covered by a ticked project but keeps other projects apps', () => {
    const out = selectionToScopes(
      selection({ projects: ['p1'], apps: ['a1', 'a2', 'b1'] }),
      ORG_ID,
      { a1: 'p1', a2: 'p1', b1: 'p2' },
    );
    expect(out).toEqual([
      { scope_type: 'project', scope_id: 'p1' },
      { scope_type: 'app', scope_id: 'b1' },
    ]);
  });

  it('keeps an app whose parent is unknown to the map', () => {
    const out = selectionToScopes(selection({ projects: ['p1'], apps: ['a1'] }), ORG_ID, {
      b1: 'p2',
    });
    expect(out).toEqual([
      { scope_type: 'project', scope_id: 'p1' },
      { scope_type: 'app', scope_id: 'a1' },
    ]);
  });

  it('keeps every ticked app when no parent map is supplied', () => {
    const out = selectionToScopes(selection({ projects: ['p1'], apps: ['a1'] }), ORG_ID);
    expect(out).toEqual([
      { scope_type: 'project', scope_id: 'p1' },
      { scope_type: 'app', scope_id: 'a1' },
    ]);
  });

  it('emits org, then projects, then apps', () => {
    const out = selectionToScopes(selection({ projects: ['p2', 'p1'], apps: ['a9'] }), ORG_ID, {
      a9: 'p3',
    });
    expect(out.map((s) => `${s.scope_type}:${s.scope_id}`)).toEqual([
      'project:p2',
      'project:p1',
      'app:a9',
    ]);
  });

  it('does not mutate the selection it is given', () => {
    const sel = selection({ projects: ['p1'], apps: ['a1'] });
    selectionToScopes(sel, ORG_ID, { a1: 'p1' });
    expect(sel).toEqual({ org: false, projects: ['p1'], apps: ['a1'], envs: [] });
  });
});

describe('selectionToScopes with environments', () => {
  it('emits one env scope per ticked env, after apps', () => {
    const out = selectionToScopes(selection({ apps: ['a1'], envs: ['e1'] }), ORG_ID);
    expect(out).toEqual([
      { scope_type: 'app', scope_id: 'a1' },
      { scope_type: 'env', scope_id: 'e1' },
    ]);
  });

  it('drops an env covered by its ticked parent app', () => {
    const out = selectionToScopes(
      selection({ apps: ['a1'], envs: ['e1', 'e2'] }),
      ORG_ID,
      { a1: 'p1' },
      { e1: 'a1', e2: 'a2' },
    );
    expect(out).toEqual([
      { scope_type: 'app', scope_id: 'a1' },
      { scope_type: 'env', scope_id: 'e2' },
    ]);
  });

  it('drops an env covered by its ticked grandparent project', () => {
    const out = selectionToScopes(
      selection({ projects: ['p1'], envs: ['e1'] }),
      ORG_ID,
      { a1: 'p1' },
      { e1: 'a1' },
    );
    expect(out).toEqual([{ scope_type: 'project', scope_id: 'p1' }]);
  });

  it('drops every narrower scope when the org is ticked', () => {
    const out = selectionToScopes(
      selection({ org: true, projects: ['p1'], apps: ['a1'], envs: ['e1'] }),
      ORG_ID,
    );
    expect(out).toEqual([{ scope_type: 'org', scope_id: ORG_ID }]);
  });
});

describe('projectCheckState', () => {
  it('is checked when the project itself is ticked', () => {
    expect(projectCheckState(selection({ projects: ['p1'] }), 'p1', ['a1'])).toBe('checked');
  });

  it('is checked even when none of its apps are ticked', () => {
    expect(projectCheckState(selection({ projects: ['p1'] }), 'p1', [])).toBe('checked');
  });

  it('is indeterminate when only some of its apps are ticked', () => {
    expect(projectCheckState(selection({ apps: ['a1'] }), 'p1', ['a1', 'a2'])).toBe(
      'indeterminate',
    );
  });

  it('is indeterminate when every one of its apps is ticked individually', () => {
    expect(projectCheckState(selection({ apps: ['a1', 'a2'] }), 'p1', ['a1', 'a2'])).toBe(
      'indeterminate',
    );
  });

  it('is unchecked when nothing under it is ticked', () => {
    expect(projectCheckState(selection({ apps: ['b1'] }), 'p1', ['a1'])).toBe('unchecked');
  });

  it('is unchecked for a project with no apps', () => {
    expect(projectCheckState(selection({ apps: ['a1'] }), 'p1', [])).toBe('unchecked');
  });

  it('ignores a sibling project being ticked', () => {
    expect(projectCheckState(selection({ projects: ['p2'] }), 'p1', ['a1'])).toBe('unchecked');
  });
});

describe('isEmptySelection', () => {
  it('is true for a fresh selection', () => {
    expect(isEmptySelection(selection())).toBe(true);
  });

  it('is false when the org is ticked', () => {
    expect(isEmptySelection(selection({ org: true }))).toBe(false);
  });

  it('is false when a project is ticked', () => {
    expect(isEmptySelection(selection({ projects: ['p1'] }))).toBe(false);
  });

  it('is false when an app is ticked', () => {
    expect(isEmptySelection(selection({ apps: ['a1'] }))).toBe(false);
  });
});

describe('isImpliedByAncestor', () => {
  it('implies every project row when the org is ticked', () => {
    expect(isImpliedByAncestor(selection({ org: true }), 'project', 'p1')).toBe(true);
  });

  it('implies every app row when the org is ticked', () => {
    expect(isImpliedByAncestor(selection({ org: true }), 'app', 'p1')).toBe(true);
  });

  it('does not imply a project row from a project tick', () => {
    expect(isImpliedByAncestor(selection({ projects: ['p1'] }), 'project', 'p1')).toBe(false);
  });

  it('implies an app row whose project is ticked', () => {
    expect(isImpliedByAncestor(selection({ projects: ['p1'] }), 'app', 'p1')).toBe(true);
  });

  it('does not imply an app row under an unticked project', () => {
    expect(isImpliedByAncestor(selection({ projects: ['p2'] }), 'app', 'p1')).toBe(false);
  });

  it('implies nothing for an empty selection', () => {
    expect(isImpliedByAncestor(selection(), 'project', 'p1')).toBe(false);
    expect(isImpliedByAncestor(selection(), 'app', 'p1')).toBe(false);
  });
});

describe('isImpliedByAncestor with environments', () => {
  it('treats an env as implied by its app, its project, or the org', () => {
    expect(isImpliedByAncestor({ org: true, projects: [], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(true);
    expect(isImpliedByAncestor({ org: false, projects: ['proj-1'], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(true);
    expect(isImpliedByAncestor({ org: false, projects: [], apps: ['app-1'], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(true);
    expect(isImpliedByAncestor({ org: false, projects: [], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(false);
  });

  it('does not imply an env from an unrelated app or project', () => {
    expect(isImpliedByAncestor({ org: false, projects: [], apps: ['app-2'], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(false);
    expect(isImpliedByAncestor({ org: false, projects: ['proj-2'], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(false);
  });

  it('does not imply an env when no grandparentId is supplied and no app is ticked', () => {
    expect(isImpliedByAncestor({ org: false, projects: [], apps: [], envs: [] }, 'env', 'app-1')).toBe(false);
  });
});
