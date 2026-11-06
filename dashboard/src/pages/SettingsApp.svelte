<script lang="ts">
  import { loadErrorText } from '../lib/models/load-error';
  import { t } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import StoreConnectionsCard from '../lib/components/settings/StoreConnectionsCard.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
  import { getApp, updateApp, deleteApp } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { appTypeIcon, appTypeLabel } from '../lib/utils/format';
  import type { App } from '../lib/models';
  import Freshness from '../lib/components/ui/Freshness.svelte';

  // Cached view (lib/stores/cached-view.svelte.ts): the app record paints from
  // cache on return rather than blanking to a spinner, then refreshes behind it.
  // Re-exposed under the names the template already uses, so the markup is
  // unchanged.
  //
  // `app` is a SHARED reference into the cache — replace it (by reloading),
  // never edit fields through it.
  const view = new CachedView<App>();

  const app = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  let togglingIngest = $state(false);
  let confirmDelete = $state(false);
  let deleting = $state(false);

  // apps.rs:71,106 both use the STRICT `authorize_app`: an env-scoped grant can
  // read the app (apps.rs:44 uses `authorize_app_reachable`) but must not be
  // able to rename, mute or delete it. `level: 'app'` is what enforces that
  // asymmetry client-side.
  const updateLock = $derived(
    app ? lockedBy('app:update', { app: app.id, level: 'app' }) : 'app:update',
  );
  const deleteLock = $derived(
    app ? lockedBy('app:delete', { app: app.id, level: 'app' }) : 'app:delete',
  );

  /**
   * `force` bypasses the fresh-window short-circuit — after a successful PATCH
   * the cached record is known-stale and must be re-read from the network.
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor puts on the request but which appears in no argument
   * here. Omit it and one environment's response can be served as another's.
   */
  const refresher = pageRefresher(async () => {
    const aid = sessionStore.currentAppId;
    if (aid) await load(aid, true);
  });

  async function load(appId: string, force = false) {
    await view.load(
      viewKey('settings.app', appId, sessionStore.scopeKey),
      () => getApp(appId),
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes — it is
    // part of the cache key, so without this the page would keep showing the
    // record fetched under the previous scope.
    sessionStore.scopeKey;
    // `idle()` on the else branch, not nothing: `CachedView.loading` starts
    // true and only a completed load clears it, so without this the page
    // spins forever on a request never issued and the "No app selected"
    // empty state below is unreachable.
    if (aid) void load(aid);
    else view.idle();
  });

  async function toggleIngest() {
    if (!app || togglingIngest) return;
    togglingIngest = true;
    const appId = app.id;
    const next = !app.ingest_enabled;
    try {
      const updated = await updateApp(appId, { ingest_enabled: next });
      sessionStore.upsertApp(updated, false);
      toastStore.success(next ? t('settings.toast.ingestEnabled') : t('settings.toast.ingestDisabled'));
      // `app` reads out of the cache now, so it can't be assigned — and hand-
      // writing the PATCH's response body into the read cache would be caching a
      // mutation. Force a re-read instead. The cached record stays on screen
      // while it runs (a cache hit means `loading` never flips), so the cards
      // don't blank, and `load` never throws — a failed refresh leaves the old
      // record up rather than turning a successful toggle into an error.
      // Prefix-wide, not just this key. The key carries `scopeKey`
      // (`appId:envId`), so this app has one cache entry PER ENVIRONMENT even
      // though the endpoint takes no environment argument. A forced reload
      // refreshes only the entry for the environment currently selected;
      // switching environments afterwards would paint the pre-mutation copy.
      viewCache.invalidate('settings.app');
      await load(appId, true);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      togglingIngest = false;
    }
  }

  async function doDelete() {
    if (!app || deleting) return;
    deleting = true;
    try {
      const id = app.id;
      await deleteApp(id);
      sessionStore.removeApp(id);
      toastStore.success(t('settings.toast.appDeleted'));
      push('/admin/projects');
    } catch (err) {
      toastStore.error(errorMessage(err));
      deleting = false;
    }
  }
</script>

<AdminShell>
  <PageHeader title={t('settings.title')}>
    {#snippet meta()}
      {#if app}
        <span class="app-badge"><Icon name={appTypeIcon(app.app_type)} size={14} /> {app.name}</span>
        <span>· {appTypeLabel(app.app_type)}</span>
      {/if}
    {/snippet}
    {#snippet freshness()}
      <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || view.revalidating} />
    {/snippet}
  </PageHeader>

  {#if view.error && view.data !== undefined}
    <!-- A forced Refresh failed over the record we already had: it stays. -->
    <div class="stale">
      <InlineNotice tone="warning">
        {t('ui.staleResults', { error: loadErrorText(view) })}
        {#snippet action()}
          <Button variant="ghost" size="sm" onclick={refresher.run}>{t('common.retry')}</Button>
        {/snippet}
      </InlineNotice>
    </div>
  {/if}

  {#if loading}
    <Skeleton rows={6} />
  {:else if view.error && view.data === undefined}
    <ErrorState
      title={t('common.couldntLoad', { thing: t('settings.thing') })}
      error={view}
      onretry={refresher.run}
    />
  {:else if !app}
    <EmptyState
      title={t('settings.noApp.title')}
      description={t('settings.noApp.body')}
      icon="package"
    >
      {#snippet action()}
        <Button variant="primary" onclick={() => push('/admin/projects')}>{t('settings.goToProjects')}</Button>
      {/snippet}
    </EmptyState>
  {:else}
    <div class="settings-stack">
      <Card title={t('settings.card.ingest')}>
          <p class="card-desc muted">
            {app.ingest_enabled ? t('settings.ingestOnBody') : t('settings.ingestOffBody')}
          </p>
        <Button
          variant={app.ingest_enabled ? 'secondary' : 'primary'}
          loading={togglingIngest}
          lockedReason={updateLock}
          onclick={toggleIngest}
        >
          {app.ingest_enabled ? t('settings.disableIngest') : t('settings.enableIngest')}
        </Button>
      </Card>

      <StoreConnectionsCard
        {app}
        onAppUpdated={(updated) => {
          sessionStore.upsertApp(updated, false);
          // Prefix-wide, for the same reason `toggleIngest` does it: the cache
          // key carries `scopeKey`, so this app has one entry PER ENVIRONMENT
          // and refreshing only the selected one would repaint the pre-mutation
          // copy after an environment switch.
          viewCache.invalidate('settings.app');
          void load(updated.id, true);
        }}
      />

      <Card title={t('settings.deleteApp')}>
          <p class="card-desc muted">
            {t('settings.deleteWarning')}
          </p>
          <Button
            variant="danger"
            lockedReason={deleteLock}
            onclick={() => (confirmDelete = true)}
          >
            {t('settings.deleteApp')}
          </Button>
      </Card>
    </div>

    <ConfirmDialog
      open={confirmDelete}
      title={t('settings.confirmDeleteTitle')}
      message={t('settings.confirmDeleteBody', { name: app.name })}
      confirmLabel={t('settings.deleteApp')}
      danger
      loading={deleting}
      onconfirm={doDelete}
      oncancel={() => (confirmDelete = false)}
    />
  {/if}
</AdminShell>

<style>
  .stale {
    margin-bottom: 16px;
  }
  .app-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: var(--weight-medium);
    color: var(--text);
  }
  .settings-stack {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
    max-width: 760px;
  }
  .card-desc {
    font-size: var(--text-sm);
    margin-bottom: 14px;
    line-height: 1.55;
  }
</style>
