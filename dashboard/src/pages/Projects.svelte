<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { createProject, updateProject, deleteProject } from '../lib/api/projects';
  import { listApps, createApp } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { appTypeIcon, appTypeLabel, APP_TYPES } from '../lib/utils/format';
  import type { App, AppType } from '../lib/models';

  // Apps loaded per project (lazily on expand).
  let appsByProject = $state<Record<string, App[]>>({});
  let loadingApps = $state<Record<string, boolean>>({});
  let openProject = $state<Record<string, boolean>>({});

  // New project form
  let showNewProject = $state(false);
  let newProjectName = $state('');
  let creatingProject = $state(false);

  // Rename state (projectId currently being renamed)
  let renamingId = $state<string | null>(null);
  let renameValue = $state('');
  let savingRename = $state(false);

  // Delete confirm state
  let confirmDeleteId = $state<string | null>(null);
  let deletingId = $state<string | null>(null);

  // New app form, keyed by project
  let newAppFor = $state<string | null>(null);
  let newAppName = $state('');
  let newAppType = $state<AppType>('web');
  let creatingApp = $state(false);

  const canCreateProject = $derived(sessionStore.can('project:create'));

  async function toggleProject(projectId: string) {
    openProject[projectId] = !openProject[projectId];
    if (openProject[projectId] && appsByProject[projectId] === undefined) {
      await loadApps(projectId);
    }
  }

  async function loadApps(projectId: string) {
    loadingApps[projectId] = true;
    try {
      appsByProject[projectId] = await listApps(projectId);
    } catch (err) {
      toastStore.error(errorMessage(err));
      appsByProject[projectId] = [];
    } finally {
      loadingApps[projectId] = false;
    }
  }

  async function submitNewProject(event: SubmitEvent) {
    event.preventDefault();
    const org = sessionStore.currentOrgId;
    if (!org || creatingProject || !newProjectName.trim()) return;
    creatingProject = true;
    try {
      const p = await createProject(org, { name: newProjectName.trim() });
      sessionStore.upsertProject(p, false);
      newProjectName = '';
      showNewProject = false;
      openProject[p.id] = true;
      appsByProject[p.id] = [];
      toastStore.success('Project created.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      creatingProject = false;
    }
  }

  function startRename(projectId: string, current: string) {
    renamingId = projectId;
    renameValue = current;
  }

  async function submitRename(projectId: string) {
    if (savingRename || !renameValue.trim()) return;
    savingRename = true;
    try {
      const updated = await updateProject(projectId, { name: renameValue.trim() });
      sessionStore.upsertProject(updated, false);
      renamingId = null;
      toastStore.success('Project renamed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      savingRename = false;
    }
  }

  async function doDelete(projectId: string) {
    if (deletingId) return;
    deletingId = projectId;
    try {
      await deleteProject(projectId);
      sessionStore.removeProject(projectId);
      delete appsByProject[projectId];
      confirmDeleteId = null;
      toastStore.success('Project deleted.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      deletingId = null;
    }
  }

  function startNewApp(projectId: string) {
    newAppFor = projectId;
    newAppName = '';
    newAppType = 'web';
  }

  async function submitNewApp(event: SubmitEvent, projectId: string) {
    event.preventDefault();
    if (creatingApp || !newAppName.trim()) return;
    creatingApp = true;
    try {
      const a = await createApp(projectId, { name: newAppName.trim(), app_type: newAppType });
      appsByProject[projectId] = [...(appsByProject[projectId] ?? []), a];
      if (projectId === sessionStore.currentProjectId) sessionStore.upsertApp(a, false);
      newAppFor = null;
      toastStore.success('App created.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      creatingApp = false;
    }
  }

  async function openApp(projectId: string, appId: string) {
    await sessionStore.selectApp(projectId, appId);
    push('/issues');
  }

  async function openAppSettings(projectId: string, appId: string) {
    await sessionStore.selectApp(projectId, appId);
    push('/settings');
  }
</script>

<AppShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">Projects</h1>
      <p class="muted sub">Group your apps by product or team. Each app holds its own DSN.</p>
    </div>
    {#if canCreateProject}
      <Button variant="primary" onclick={() => (showNewProject = !showNewProject)}>
        {showNewProject ? 'Cancel' : 'New project'}
      </Button>
    {/if}
  </div>

  {#if showNewProject}
    <Card class="new-project">
      <form class="inline-form" onsubmit={submitNewProject}>
        <Input label="Project name" bind:value={newProjectName} placeholder="Payments" required />
        <Button type="submit" variant="primary" loading={creatingProject}>Create project</Button>
      </form>
    </Card>
  {/if}

  {#if sessionStore.projects.length === 0}
    <Card>
      <EmptyState
        title="No projects yet"
        description="Create a project to start grouping apps."
        icon="folders"
      >
        {#snippet action()}
          {#if canCreateProject}
            <Button variant="primary" onclick={() => (showNewProject = true)}>New project</Button>
          {/if}
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <div class="project-list">
      {#each sessionStore.projects as project (project.id)}
        {@const canUpdate = sessionStore.can('project:update', { project: project.id })}
        {@const canDelete = sessionStore.can('project:delete', { project: project.id })}
        {@const canCreateApp = sessionStore.can('app:create', { project: project.id })}
        <Card padding="none" class="project-card">
          <div class="project-row">
            <button
              class="expander"
              onclick={() => toggleProject(project.id)}
              aria-expanded={!!openProject[project.id]}
              aria-label="Toggle apps"
            >
              <span class="chevron" class:open={openProject[project.id]}><Icon name="chevron-right" size={14} /></span>
            </button>

            <div class="p-main">
              {#if renamingId === project.id}
                <form
                  class="rename-form"
                  onsubmit={(e) => {
                    e.preventDefault();
                    submitRename(project.id);
                  }}
                >
                  <Input bind:value={renameValue} required />
                  <Button type="submit" variant="primary" size="sm" loading={savingRename}>Save</Button>
                  <Button variant="ghost" size="sm" onclick={() => (renamingId = null)}>Cancel</Button>
                </form>
              {:else}
                <button class="p-name-btn" onclick={() => toggleProject(project.id)}>
                  <span class="p-name">{project.name}</span>
                  <span class="p-slug mono">{project.slug}</span>
                </button>
              {/if}
            </div>

            {#if renamingId !== project.id}
              <div class="p-actions">
                {#if canUpdate}
                  <Button variant="ghost" size="sm" onclick={() => startRename(project.id, project.name)}>
                    Rename
                  </Button>
                {/if}
                {#if canDelete}
                  <Button variant="ghost" size="sm" onclick={() => (confirmDeleteId = project.id)}>
                    Delete
                  </Button>
                {/if}
              </div>
            {/if}
          </div>

          {#if confirmDeleteId === project.id}
            <div class="confirm">
              <span class="confirm-text">
                Delete <strong>{project.name}</strong> and every app beneath it?
              </span>
              <div class="confirm-actions">
                <Button
                  variant="danger"
                  size="sm"
                  loading={deletingId === project.id}
                  onclick={() => doDelete(project.id)}
                >
                  Yes, delete
                </Button>
                <Button variant="ghost" size="sm" onclick={() => (confirmDeleteId = null)}>Cancel</Button>
              </div>
            </div>
          {/if}

          {#if openProject[project.id]}
            <div class="apps">
              {#if loadingApps[project.id]}
                <div class="apps-loading"><Spinner size={18} /></div>
              {:else}
                {#each appsByProject[project.id] ?? [] as app (app.id)}
                  <div class="app-row">
                    <span class="a-icon" aria-hidden="true"><Icon name={appTypeIcon(app.app_type)} size={15} /></span>
                    <div class="a-meta">
                      <span class="a-name">{app.name}</span>
                      <span class="a-type muted">{appTypeLabel(app.app_type)}</span>
                    </div>
                    <Badge tone={app.ingest_enabled ? 'success' : 'neutral'} size="sm" dot>
                      {app.ingest_enabled ? 'ingesting' : 'paused'}
                    </Badge>
                    <div class="a-actions">
                      <Button variant="subtle" size="sm" onclick={() => openApp(project.id, app.id)}>
                        Open
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onclick={() => openAppSettings(project.id, app.id)}
                      >
                        Settings
                      </Button>
                    </div>
                  </div>
                {:else}
                  <p class="no-apps muted">No apps in this project yet.</p>
                {/each}

                {#if canCreateApp}
                  {#if newAppFor === project.id}
                    <form class="new-app-form" onsubmit={(e) => submitNewApp(e, project.id)}>
                      <Input bind:value={newAppName} placeholder="App name" required />
                      <select class="type-select" bind:value={newAppType} aria-label="App type">
                        {#each APP_TYPES as t (t.value)}
                          <option value={t.value}>{t.label}</option>
                        {/each}
                      </select>
                      <Button type="submit" variant="primary" size="sm" loading={creatingApp}>
                        Create app
                      </Button>
                      <Button variant="ghost" size="sm" onclick={() => (newAppFor = null)}>Cancel</Button>
                    </form>
                  {:else}
                    <button class="add-app" onclick={() => startNewApp(project.id)}>+ New app</button>
                  {/if}
                {/if}
              {/if}
            </div>
          {/if}
        </Card>
      {/each}
    </div>
  {/if}
</AppShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  :global(.new-project) {
    margin-bottom: 16px;
  }
  .inline-form {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    flex-wrap: wrap;
  }
  .inline-form :global(.field) {
    flex: 1;
    min-width: 220px;
  }
  .project-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .project-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 16px;
  }
  .expander {
    background: none;
    border: none;
    padding: 4px;
    color: var(--text-faint);
    display: grid;
    place-items: center;
  }
  .chevron {
    display: inline-block;
    transition: transform 0.15s ease;
    font-size: 12px;
  }
  .chevron.open {
    transform: rotate(90deg);
  }
  .p-main {
    flex: 1;
    min-width: 0;
  }
  .p-name-btn {
    display: flex;
    align-items: baseline;
    gap: 10px;
    background: none;
    border: none;
    padding: 0;
    text-align: left;
    min-width: 0;
  }
  .p-name {
    font-weight: 600;
    font-size: 14.5px;
    color: var(--text);
  }
  .p-slug {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .rename-form {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .rename-form :global(.field) {
    min-width: 200px;
  }
  .p-actions {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }
  .confirm {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 16px;
    background: var(--error-soft);
    border-top: 1px solid color-mix(in srgb, var(--error) 25%, transparent);
    flex-wrap: wrap;
  }
  .confirm-text {
    font-size: 13px;
  }
  .confirm-actions {
    display: flex;
    gap: 8px;
  }
  .apps {
    border-top: 1px solid var(--border);
    padding: 6px 16px 14px 42px;
    display: flex;
    flex-direction: column;
  }
  .apps-loading {
    padding: 16px 0;
  }
  .app-row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
  }
  .app-row:last-of-type {
    border-bottom: none;
  }
  .a-icon {
    font-size: 16px;
    line-height: 1;
    width: 22px;
    text-align: center;
  }
  .a-meta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
  }
  .a-name {
    font-weight: 560;
    font-size: 13.5px;
  }
  .a-type {
    font-size: 11.5px;
  }
  .a-actions {
    display: flex;
    gap: 4px;
  }
  .no-apps {
    font-size: 13px;
    padding: 10px 0;
  }
  .new-app-form {
    display: flex;
    align-items: center;
    gap: 8px;
    padding-top: 12px;
    flex-wrap: wrap;
  }
  .new-app-form :global(.field) {
    min-width: 180px;
  }
  .type-select {
    padding: 9px 11px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-size: 13.5px;
    outline: none;
  }
  .type-select option {
    background: var(--surface);
    color: var(--text);
  }
  .add-app {
    align-self: flex-start;
    margin-top: 10px;
    background: none;
    border: 1px dashed var(--border-strong);
    border-radius: var(--radius);
    color: var(--text-muted);
    padding: 8px 14px;
    font-size: 13px;
    font-weight: 540;
    transition: all 0.13s ease;
  }
  .add-app:hover {
    color: var(--text);
    border-color: var(--text-faint);
  }
</style>
