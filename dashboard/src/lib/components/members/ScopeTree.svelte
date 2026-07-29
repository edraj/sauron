<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import {
    describeSelection,
    isImpliedByAncestor,
    projectCheckState,
    type ScopeSelection,
  } from '../../models/scope-tree';

  interface Props {
    orgId: string;
    orgName: string;
    projects: { id: string; name: string }[];
    appsByProject: Record<string, { id: string; name: string }[]>;
    value: ScopeSelection;
    disabled?: boolean;
    onchange: (next: ScopeSelection) => void;
  }

  let {
    orgId,
    orgName,
    projects,
    appsByProject,
    value,
    disabled = false,
    onchange,
  }: Props = $props();

  /** Disclosure state the admin set by hand; the rest falls back to isOpen(). */
  let opened = $state<Record<string, boolean>>({});

  const projectOfApp = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const p of projects) for (const a of appsByProject[p.id] ?? []) map[a.id] = p.id;
    return map;
  });

  const summary = $derived(describeSelection(value, orgId, orgName, projectOfApp));

  function appsOf(projectId: string) {
    return appsByProject[projectId] ?? [];
  }

  // Open by default when something inside is already ticked, so an existing
  // selection is never hidden behind a collapsed row.
  function isOpen(projectId: string): boolean {
    return opened[projectId] ?? appsOf(projectId).some((a) => value.apps.includes(a.id));
  }

  function toggleOpen(projectId: string) {
    opened = { ...opened, [projectId]: !isOpen(projectId) };
  }

  // Narrower picks are kept rather than dropped: selectionToScopes discards
  // them while the org is ticked anyway, so unticking restores what was there.
  function toggleOrg() {
    if (disabled) return;
    onchange({ ...value, org: !value.org });
  }

  function toggleProject(projectId: string) {
    if (disabled || isImpliedByAncestor(value, 'project', projectId)) return;
    if (value.projects.includes(projectId)) {
      onchange({ ...value, projects: value.projects.filter((id) => id !== projectId) });
      return;
    }
    // A project grant already covers its apps, so ticking it absorbs them —
    // otherwise unticking the project later would leave orphaned app grants.
    const under = new Set(appsOf(projectId).map((a) => a.id));
    onchange({
      ...value,
      projects: [...value.projects, projectId],
      apps: value.apps.filter((id) => !under.has(id)),
    });
  }

  function toggleApp(appId: string, projectId: string) {
    if (disabled || isImpliedByAncestor(value, 'app', projectId)) return;
    onchange({
      ...value,
      apps: value.apps.includes(appId)
        ? value.apps.filter((id) => id !== appId)
        : [...value.apps, appId],
    });
  }
</script>

<div class="scope-tree" class:disabled role="group" aria-label="Access scope">
  <div class="tree">
    <div class="row">
      <span class="twisty-gap"></span>
      <label class="node">
        <input type="checkbox" checked={value.org} {disabled} onchange={toggleOrg} />
        <span class="n-name">{orgName}</span>
        <span class="n-hint">entire org</span>
      </label>
    </div>

    {#each projects as project (project.id)}
      {@const apps = appsOf(project.id)}
      {@const implied = isImpliedByAncestor(value, 'project', project.id)}
      {@const state = projectCheckState(
        value,
        project.id,
        apps.map((a) => a.id),
      )}
      {@const open = isOpen(project.id)}
      <div class="row lvl-1" class:implied>
        {#if apps.length}
          <button
            type="button"
            class="twisty"
            aria-expanded={open}
            aria-label={`${open ? 'Collapse' : 'Expand'} ${project.name}`}
            onclick={() => toggleOpen(project.id)}
          >
            <Icon name={open ? 'chevron-down' : 'chevron-right'} size={13} />
          </button>
        {:else}
          <span class="twisty-gap"></span>
        {/if}
        <label class="node">
          <input
            type="checkbox"
            checked={implied || state === 'checked'}
            indeterminate={!implied && state === 'indeterminate'}
            disabled={disabled || implied}
            onchange={() => toggleProject(project.id)}
          />
          <span class="n-name">{project.name}</span>
          {#if apps.length}
            <span class="n-hint">{apps.length} app{apps.length === 1 ? '' : 's'}</span>
          {/if}
        </label>
      </div>

      {#if open}
        {#each apps as app (app.id)}
          {@const appImplied = isImpliedByAncestor(value, 'app', project.id)}
          <div class="row lvl-2" class:implied={appImplied}>
            <span class="twisty-gap"></span>
            <label class="node">
              <input
                type="checkbox"
                checked={appImplied || value.apps.includes(app.id)}
                disabled={disabled || appImplied}
                onchange={() => toggleApp(app.id, project.id)}
              />
              <span class="n-name">{app.name}</span>
            </label>
          </div>
        {/each}
      {/if}
    {/each}

    {#if projects.length === 0}
      <p class="empty">No projects yet — the whole org is the only scope.</p>
    {/if}
  </div>
  <p class="summary">{summary}</p>
</div>

<style>
  .scope-tree {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .tree {
    display: flex;
    flex-direction: column;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 8px 10px;
    /* A big org must scroll inside the picker rather than grow the modal past
       its own max-height and push the footer buttons off screen. */
    max-height: 240px;
    overflow-y: auto;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: 26px;
  }
  .lvl-1 {
    padding-left: 14px;
  }
  .lvl-2 {
    padding-left: 34px;
  }
  .row.implied {
    opacity: 0.55;
  }
  .twisty {
    display: inline-grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    flex-shrink: 0;
  }
  .twisty:hover {
    color: var(--text);
  }
  .twisty-gap {
    width: 18px;
    flex-shrink: 0;
  }
  .node {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font-size: 13px;
    color: var(--text);
    cursor: pointer;
  }
  .node:has(input:disabled) {
    cursor: default;
  }
  .node input {
    accent-color: var(--primary);
    flex-shrink: 0;
  }
  .n-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .n-hint {
    font-size: 11.5px;
    color: var(--text-faint);
    white-space: nowrap;
  }
  .empty {
    font-size: 12.5px;
    color: var(--text-faint);
    padding: 4px 0 2px 14px;
  }
  .summary {
    font-size: 12px;
    color: var(--text-muted);
  }
  /* Disclosure stays live while disabled so the admin can still read the tree. */
  .scope-tree.disabled .node {
    cursor: not-allowed;
  }
</style>
