<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import Spinner from '../ui/Spinner.svelte';
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
    /**
     * Environments per app, keyed by app id. Presence of a key — even an empty
     * array — means "loaded"; absence means the parent has not fetched it yet.
     * An org can have hundreds of apps, so this is never populated eagerly for
     * every app up front; see `onopenapp`.
     */
    envsByApp: Record<string, { id: string; name: string }[]>;
    /** App ids whose environment list is currently in flight, so that row can
        show a spinner instead of a stale/empty twisty. */
    loadingEnvApps?: Set<string>;
    value: ScopeSelection;
    disabled?: boolean;
    /**
     * Whether the "entire org" row is offered. Defaults to true so Members and
     * EditMember are unchanged; the subscription dialog passes false, because
     * one org tick would fan a subscription out to every app in the org.
     */
    allowOrg?: boolean;
    /**
     * Whether the environment level is offered under each app. Defaults to
     * true. The subscription dialog passes false: these rows are
     * `AppEnvironment.id` — ENROLLMENT ids — while a subscription stores
     * CATALOGUE ids in its own chip row, and rendering both with identical
     * labels would put two id spaces in one form.
     */
    allowEnv?: boolean;
    onchange: (next: ScopeSelection) => void;
    /**
     * Fired when an app row's disclosure opens and `envsByApp` has no entry
     * for it yet. The caller owns the network call and the cache — an org
     * with hundreds of apps must not pay for hundreds of requests just
     * because a dialog opened, so environments are only ever asked for one
     * app at a time, on demand, the first time its row is expanded.
     */
    onopenapp: (appId: string) => void;
  }

  let {
    orgId,
    orgName,
    projects,
    appsByProject,
    envsByApp,
    loadingEnvApps = new Set<string>(),
    value,
    disabled = false,
    allowOrg = true,
    allowEnv = true,
    onchange,
    onopenapp,
  }: Props = $props();

  /** Disclosure state the admin set by hand; the rest falls back to isXOpen(). */
  let openedProjects = $state<Record<string, boolean>>({});
  let openedApps = $state<Record<string, boolean>>({});

  const projectOfApp = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const p of projects) for (const a of appsByProject[p.id] ?? []) map[a.id] = p.id;
    return map;
  });

  const appOfEnv = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const [appId, envs] of Object.entries(envsByApp)) {
      for (const e of envs) map[e.id] = appId;
    }
    return map;
  });

  const summary = $derived(describeSelection(value, orgId, orgName, projectOfApp, appOfEnv));

  function appsOf(projectId: string) {
    return appsByProject[projectId] ?? [];
  }

  function envsOf(appId: string) {
    return envsByApp[appId] ?? [];
  }

  /** Tri-state for an app row, given the envs that live under it — the same
      idea as projectCheckState one level down, kept local since it needs
      envsOf() rather than a project's static app list. */
  function appCheckState(appId: string): 'checked' | 'indeterminate' | 'unchecked' {
    if (value.apps.includes(appId)) return 'checked';
    if (envsOf(appId).some((e) => value.envs.includes(e.id))) return 'indeterminate';
    return 'unchecked';
  }

  // Open by default when something inside is already ticked, so an existing
  // selection is never hidden behind a collapsed row. This has to check two
  // levels down, not just direct app children — an edit dialog seeded from an
  // env-scoped grant ticks value.envs with nothing in value.apps at all, and
  // without the envsOf() half below, the project row (and everything under
  // it) would stay collapsed and hide the very tick this auto-open exists to
  // surface. Only reaches envs already in envsByApp — see isAppOpen's comment.
  function isProjectOpen(projectId: string): boolean {
    return (
      openedProjects[projectId] ??
      appsOf(projectId).some(
        (a) => value.apps.includes(a.id) || envsOf(a.id).some((e) => value.envs.includes(e.id)),
      )
    );
  }

  function toggleOpenProject(projectId: string) {
    openedProjects = { ...openedProjects, [projectId]: !isProjectOpen(projectId) };
  }

  // Same idea one level down: auto-open when an environment inside is already
  // ticked. That can only be true once envsByApp[appId] is loaded — ticking an
  // env requires its row to have rendered in the first place — so this never
  // needs to trigger a fetch by itself; toggleOpenApp below is what asks the
  // parent to load an app's environments the first time it is opened.
  function isAppOpen(appId: string): boolean {
    return openedApps[appId] ?? envsOf(appId).some((e) => value.envs.includes(e.id));
  }

  function toggleOpenApp(appId: string) {
    const next = !isAppOpen(appId);
    openedApps = { ...openedApps, [appId]: next };
    if (next && !(appId in envsByApp)) onopenapp(appId);
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
    // A project grant already covers its apps and, transitively, their
    // environments, so ticking it absorbs both — otherwise unticking the
    // project later would leave orphaned app and environment grants behind.
    const apps = appsOf(projectId);
    const underApps = new Set(apps.map((a) => a.id));
    const underEnvs = new Set(apps.flatMap((a) => envsOf(a.id).map((e) => e.id)));
    onchange({
      ...value,
      projects: [...value.projects, projectId],
      apps: value.apps.filter((id) => !underApps.has(id)),
      envs: value.envs.filter((id) => !underEnvs.has(id)),
    });
  }

  function toggleApp(appId: string, projectId: string) {
    if (disabled || isImpliedByAncestor(value, 'app', projectId)) return;
    if (value.apps.includes(appId)) {
      onchange({ ...value, apps: value.apps.filter((id) => id !== appId) });
      return;
    }
    // An app grant already covers its environments, so ticking it absorbs
    // them — otherwise unticking the app later would leave orphaned
    // environment grants behind, exactly as toggleProject absorbs apps (and
    // now their environments) above.
    const under = new Set(envsOf(appId).map((e) => e.id));
    onchange({
      ...value,
      apps: [...value.apps, appId],
      envs: value.envs.filter((id) => !under.has(id)),
    });
  }

  function toggleEnv(envId: string, appId: string, projectId: string) {
    if (disabled || isImpliedByAncestor(value, 'env', appId, projectId)) return;
    onchange({
      ...value,
      envs: value.envs.includes(envId)
        ? value.envs.filter((id) => id !== envId)
        : [...value.envs, envId],
    });
  }
</script>

<div class="scope-tree" class:disabled role="group" aria-label="Access scope">
  <div class="tree">
    {#if allowOrg}
      <div class="row">
        <span class="twisty-gap"></span>
        <label class="node">
          <input type="checkbox" checked={value.org} {disabled} onchange={toggleOrg} />
          <span class="n-name">{orgName}</span>
          <span class="n-hint">entire org</span>
        </label>
      </div>
    {/if}

    {#each projects as project (project.id)}
      {@const apps = appsOf(project.id)}
      {@const implied = isImpliedByAncestor(value, 'project', project.id)}
      {@const state = projectCheckState(
        value,
        project.id,
        apps.map((a) => a.id),
      )}
      {@const open = isProjectOpen(project.id)}
      <div class="row lvl-1" class:implied>
        {#if apps.length}
          <button
            type="button"
            class="twisty"
            aria-expanded={open}
            aria-label={`${open ? 'Collapse' : 'Expand'} ${project.name}`}
            onclick={() => toggleOpenProject(project.id)}
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
          {@const appState = appCheckState(app.id)}
          {@const appOpen = isAppOpen(app.id)}
          {@const envs = envsOf(app.id)}
          {@const envsLoading = loadingEnvApps.has(app.id)}
          <div class="row lvl-2" class:implied={appImplied}>
            {#if allowEnv}
              <!-- Always rendered when environments are offered, unlike the
                   project twisty above: an app's env count is unknown until
                   fetched, so the disclosure can't be conditionally hidden the
                   way an empty project's can. With `allowEnv = false` there is
                   nothing to disclose, and an expander that opens onto nothing
                   reads as a broken control. -->
              <button
                type="button"
                class="twisty"
                aria-expanded={appOpen}
                aria-label={`${appOpen ? 'Collapse' : 'Expand'} ${app.name}`}
                onclick={() => toggleOpenApp(app.id)}
              >
                {#if envsLoading}
                  <Spinner size={11} stroke={1.5} />
                {:else}
                  <Icon name={appOpen ? 'chevron-down' : 'chevron-right'} size={13} />
                {/if}
              </button>
            {:else}
              <span class="twisty-gap"></span>
            {/if}
            <label class="node">
              <input
                type="checkbox"
                checked={appImplied || appState === 'checked'}
                indeterminate={!appImplied && appState === 'indeterminate'}
                disabled={disabled || appImplied}
                onchange={() => toggleApp(app.id, project.id)}
              />
              <span class="n-name">{app.name}</span>
              <!-- An env count under an app whose environments are not
                   selectable is a promise the tree does not keep. -->
              {#if allowEnv && envs.length}
                <span class="n-hint">{envs.length} env{envs.length === 1 ? '' : 's'}</span>
              {/if}
            </label>
          </div>

          {#if allowEnv && appOpen}
            {#if envs.length}
              {#each envs as env (env.id)}
                {@const envImplied = isImpliedByAncestor(value, 'env', app.id, project.id)}
                <div class="row lvl-3" class:implied={envImplied}>
                  <span class="twisty-gap"></span>
                  <label class="node">
                    <input
                      type="checkbox"
                      checked={envImplied || value.envs.includes(env.id)}
                      disabled={disabled || envImplied}
                      onchange={() => toggleEnv(env.id, app.id, project.id)}
                    />
                    <span class="n-name">{env.name}</span>
                  </label>
                </div>
              {/each}
            {:else if envsLoading}
              <div class="row lvl-3">
                <span class="twisty-gap"></span>
                <span class="n-hint loading-row"><Spinner size={11} stroke={1.5} /> Loading environments…</span>
              </div>
            {:else}
              <p class="empty lvl-3-empty">No environments.</p>
            {/if}
          {/if}
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
  .lvl-3 {
    padding-left: 54px;
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
  .loading-row {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .empty {
    font-size: 12.5px;
    color: var(--text-faint);
    padding: 4px 0 2px 14px;
  }
  .empty.lvl-3-empty {
    padding: 2px 0 2px 54px;
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
