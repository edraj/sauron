<script lang="ts">
  import { loadErrorText } from '../lib/models/load-error';
  import { t } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Select from '../lib/components/ui/Select.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { refreshRegistry } from '../lib/stores/refresh-registry';
  import { currentRoute } from '../lib/stores/current-route';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import { createProject, updateProject, deleteProject } from '../lib/api/projects';
  import { listApps, createApp } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { appTypeIcon, appTypeLabel, APP_TYPES } from '../lib/utils/format';
  import type { App, AppType } from '../lib/models';

  // Apps loaded per project (lazily on expand).
  let appsByProject = $state<Record<string, App[]>>({});
  let loadingApps = $state<Record<string, boolean>>({});
  // The last failed app-list load per project. `stale` says there were rows
  // to keep (a failed refresh), so the section shows them with a notice rather
  // than replacing them with the error.
  let appsError = $state<Record<string, { error: unknown; stale: boolean }>>({});
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
  const confirmDeleteProject = $derived(
    confirmDeleteId ? (sessionStore.projects.find((p) => p.id === confirmDeleteId) ?? null) : null,
  );

  // New app form, keyed by project
  let newAppFor = $state<string | null>(null);
  let newAppName = $state('');
  let newAppType = $state<AppType>('web');
  let creatingApp = $state(false);
  const appTypeOptions = APP_TYPES.map((o) => ({ value: o.value, label: o.label }));

  // projects.rs:102 authorizes creation at the org — a project- or app-scoped
  // grant carrying `project:create` cannot satisfy it, hence `level: 'org'`.
  const createProjectLock = $derived(lockedBy('project:create', { level: 'org' }));

  async function toggleProject(projectId: string) {
    openProject[projectId] = !openProject[projectId];
    if (openProject[projectId] && appsByProject[projectId] === undefined) {
      await loadApps(projectId);
    }
  }

  /**
   * Expanding a project paints its apps from cache immediately, then refreshes
   * behind them — re-expanding a row you collapsed a moment ago no longer spins.
   *
   * `viewCache` directly rather than a `CachedView`: this page holds N lists at
   * once, one per expanded project, and `CachedView` models a single payload.
   * The record already carries the per-project state a view would have supplied.
   *
   * Both assignments COPY. `viewCache.get`/`set` hand back the very array the
   * cache holds, and assigning it into `appsByProject` — a `$state` record —
   * would wrap it in Svelte's deep proxy, so a later write through the proxy
   * would reach the cached array and survive navigation. Copying severs that.
   */
  async function loadApps(projectId: string, force = false) {
    const key = viewKey('projects.apps', projectId);
    const cached = viewCache.get<App[]>(key);
    if (cached !== undefined) appsByProject[projectId] = [...cached];
    if (!force && viewCache.isFresh(key)) return;
    // Only a cache MISS shows the spinner; a revalidate leaves the rows up.
    loadingApps[projectId] = cached === undefined;
    delete appsError[projectId];
    try {
      const apps = await viewCache.dedupe(key, () => listApps(projectId), force);
      viewCache.set(key, apps);
      appsByProject[projectId] = [...apps];
    } catch (err) {
      // Said inside the project's own section (ErrorState, with Retry), not in
      // a toast that is gone before anyone reads it. Only blank the row when
      // there was nothing to keep: a failed refresh over good rows leaves them
      // up — they are still a true answer, merely older.
      appsError[projectId] = { error: err, stale: cached !== undefined };
      if (cached === undefined) appsByProject[projectId] = [];
    } finally {
      loadingApps[projectId] = false;
    }
  }

  /**
   * Reload every project whose row is currently expanded.
   *
   * Collapsed rows are deliberately skipped: their list is not on screen, and
   * fetching it would spend a request on something nobody is looking at —
   * expanding the row already refreshes it.
   */
  async function reloadOpenProjects(): Promise<void> {
    const open = Object.keys(openProject).filter((id) => openProject[id]);
    await Promise.allSettled(open.map((id) => loadApps(id, true)));
  }

  const refresher = pageRefresher(reloadOpenProjects);

  /**
   * Make this page reachable by the GLOBAL refresh in the top bar.
   *
   * Every other page is registered automatically by `CachedView.load`, but this
   * one drives `viewCache` directly — it holds N lists, one per expanded
   * project — so it owns no `CachedView` and registered nothing. The top-bar
   * button therefore did nothing at all here, silently, which is exactly the
   * invisible failure the registry's route tagging was designed to avoid.
   *
   * `pending` is always false: none of these lists is server-cached, so there
   * is never a recompute to wait for and the button must not poll.
   */
  $effect(() => {
    // `lastKey: null` — this page drives `viewCache` under N keys rather than
    // one, so it names none. Its entries are simply dropped by the global
    // refresh's invalidation and refetched when a row is next expanded.
    refreshRegistry.register(
      { reload: reloadOpenProjects, pending: false, lastKey: null },
      currentRoute(),
    );
  });

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
      toastStore.success(t('projects.toast.created'));
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
      toastStore.success(t('projects.toast.renamed'));
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
      // The project is gone; its cached app list must not outlive it and be
      // served to an id the server would now 404.
      viewCache.invalidate(viewKey('projects.apps', projectId));
      confirmDeleteId = null;
      toastStore.success(t('projects.toast.deleted'));
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
      // The row is on screen already; drop the cache entry so the next expand
      // refetches instead of serving a pre-create list for the fresh window.
      viewCache.invalidate(viewKey('projects.apps', projectId));
      if (projectId === sessionStore.currentProjectId) sessionStore.upsertApp(a, false);
      newAppFor = null;
      toastStore.success(t('projects.toast.appCreated'));
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
    push('/admin/settings');
  }
</script>

<AdminShell>
  <PageHeader title={t('projects.title')} subtitle={t('projects.subtitle')}>
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy} />
    {/snippet}
    {#snippet primary()}
      <!-- Hidden while the form is open: the form's own Cancel closes it, and
           "Create project" is then the view's one primary. -->
      {#if !showNewProject}
        <Button
          variant="primary"
          lockedReason={createProjectLock}
          onclick={() => (showNewProject = true)}
        >
          {t('projects.new')}
        </Button>
      {/if}
    {/snippet}
  </PageHeader>

  {#if showNewProject}
    <div class="new-project">
      <Card title={t('projects.new')}>
        <form class="inline-form" onsubmit={submitNewProject}>
          <Input label={t('projects.name')} bind:value={newProjectName} placeholder={t('auth.placeholder.projectName')} required />
          <div class="form-actions">
            <Button variant="secondary" onclick={() => (showNewProject = false)}>{t('common.cancel')}</Button>
            <Button type="submit" variant="primary" loading={creatingProject}>{t('projects.create')}</Button>
          </div>
        </form>
      </Card>
    </div>
  {/if}

  {#if sessionStore.projects.length === 0}
    <EmptyState
      title={t('projects.empty.title')}
      description={t('projects.empty.body')}
      icon="folders"
    >
      {#snippet action()}
        {#if !showNewProject}
          <!-- `secondary`: the header already holds this view's one primary. -->
          <Button
            variant="secondary"
            lockedReason={createProjectLock}
            onclick={() => (showNewProject = true)}
          >
            {t('projects.new')}
          </Button>
        {/if}
      {/snippet}
    </EmptyState>
  {:else}
    <div class="project-list">
      {#each sessionStore.projects as project (project.id)}
        <!-- projects.rs:146,162,239 all authorize at the project. -->
        {@const updateLock = lockedBy('project:update', { project: project.id, level: 'project' })}
        {@const deleteLock = lockedBy('project:delete', { project: project.id, level: 'project' })}
        {@const createAppLock = lockedBy('app:create', { project: project.id, level: 'project' })}
        <Card class="project-card">
          <div class="project-row">
            <button
              class="expander"
              onclick={() => toggleProject(project.id)}
              aria-expanded={!!openProject[project.id]}
              aria-label={t('projects.toggleApps')}
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
                  <Button variant="secondary" size="sm" onclick={() => (renamingId = null)}>{t('common.cancel')}</Button>
                  <Button type="submit" variant="primary" size="sm" loading={savingRename}>{t('common.save')}</Button>
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
                <Button
                  variant="ghost"
                  size="sm"
                  lockedReason={updateLock}
                  onclick={() => startRename(project.id, project.name)}
                >
                  {t('projects.rename')}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  lockedReason={deleteLock}
                  onclick={() => (confirmDeleteId = project.id)}
                >
                  {t('common.delete')}
                </Button>
              </div>
            {/if}
          </div>

          {#if openProject[project.id]}
            {@const appsFailure = appsError[project.id]}
            <div class="apps">
              {#if loadingApps[project.id]}
                <div class="apps-loading"><Skeleton rows={2} /></div>
              {:else if appsFailure && !appsFailure.stale}
                <ErrorState
                  size="section"
                  title={t('common.couldntLoad', { thing: t('projects.appsThing') })}
                  error={appsFailure.error}
                  onretry={() => loadApps(project.id, true)}
                />
              {:else}
                {#if appsFailure}
                  <div class="apps-stale">
                    <InlineNotice tone="warning">
                      {t('ui.staleResults', { error: loadErrorText(appsFailure.error) })}
                      {#snippet action()}
                        <Button variant="ghost" size="sm" onclick={() => loadApps(project.id, true)}>
                          {t('common.retry')}
                        </Button>
                      {/snippet}
                    </InlineNotice>
                  </div>
                {/if}
                {#each appsByProject[project.id] ?? [] as app (app.id)}
                  <div class="app-row">
                    <span class="a-icon" aria-hidden="true"><Icon name={appTypeIcon(app.app_type)} size={15} /></span>
                    <div class="a-meta">
                      <span class="a-name">{app.name}</span>
                      <span class="a-type muted">{appTypeLabel(app.app_type)}</span>
                    </div>
                    <Badge tone={app.ingest_enabled ? 'success' : 'neutral'} size="sm" dot>
                      {app.ingest_enabled ? t('projects.ingesting') : t('projects.paused')}
                    </Badge>
                    <div class="a-actions">
                      <Button variant="ghost" size="sm" onclick={() => openApp(project.id, app.id)}>
                        {t('projects.open')}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onclick={() => openAppSettings(project.id, app.id)}
                      >
                        {t('projects.settings')}
                      </Button>
                    </div>
                  </div>
                {:else}
                  <p class="no-apps muted">{t('projects.noApps')}</p>
                {/each}

                  {#if newAppFor === project.id}
                    <form class="new-app-form" onsubmit={(e) => submitNewApp(e, project.id)}>
                      <Input bind:value={newAppName} placeholder={t('projects.appName')} required />
                      <Select
                        value={newAppType}
                        options={appTypeOptions}
                        ariaLabel={t('projects.appType')}
                        onchange={(v) => (newAppType = v as AppType)}
                      />
                      <Button variant="secondary" size="sm" onclick={() => (newAppFor = null)}>{t('common.cancel')}</Button>
                      <Button
                        type="submit"
                        variant="primary"
                        size="sm"
                        loading={creatingApp}
                        lockedReason={createAppLock}
                      >
                        {t('projects.createApp')}
                      </Button>
                    </form>
                  {:else}
                    <div class="add-app">
                      <Button
                        variant="secondary"
                        size="sm"
                        lockedReason={createAppLock}
                        onclick={() => startNewApp(project.id)}
                      >
                        {t('projects.newApp')}
                      </Button>
                    </div>
                  {/if}
              {/if}
            </div>
          {/if}
        </Card>
      {/each}
    </div>
  {/if}

  <ConfirmDialog
    open={confirmDeleteProject !== null}
    title={t('projects.confirmDeleteTitle')}
    message={t('projects.confirmDeleteBody', { name: confirmDeleteProject?.name ?? '' })}
    confirmLabel={t('projects.deleteAction')}
    danger
    loading={deletingId !== null && deletingId === confirmDeleteId}
    onconfirm={() => confirmDeleteId && doDelete(confirmDeleteId)}
    oncancel={() => (confirmDeleteId = null)}
  />
</AdminShell>

<style>
  .new-project {
    margin-bottom: var(--page-gap);
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
  .form-actions {
    display: flex;
    gap: 8px;
  }
  .project-list {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .project-row {
    display: flex;
    align-items: center;
    gap: 10px;
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
    transition: transform 0.12s ease;
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
    text-align: start;
    min-width: 0;
  }
  .p-name {
    font-weight: var(--weight-strong);
    font-size: var(--text-md);
    color: var(--text);
  }
  .p-slug {
    font-size: var(--text-xs);
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
  .apps {
    margin-top: 10px;
    padding-inline-start: 32px;
    display: flex;
    flex-direction: column;
  }
  .apps-stale {
    margin: 6px 0;
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
    font-size: var(--text-md);
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
    font-weight: var(--weight-medium);
    font-size: var(--text-sm);
  }
  .a-type {
    font-size: var(--text-xs);
  }
  .a-actions {
    display: flex;
    gap: 4px;
  }
  .no-apps {
    font-size: var(--text-sm);
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
  .add-app {
    align-self: flex-start;
    margin-top: 10px;
  }
</style>
