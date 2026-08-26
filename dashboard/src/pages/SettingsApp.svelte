<script lang="ts">
  import { t } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import StoreConnectionsCard from '../lib/components/settings/StoreConnectionsCard.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
  import { getApp, updateApp, deleteApp } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { appTypeIcon, appTypeLabel } from '../lib/utils/format';
  import type { App } from '../lib/models';

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
  const error = $derived(view.error);
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
      toastStore.success(next ? 'Ingest enabled.' : 'Ingest disabled.');
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
      toastStore.success('App deleted.');
      push('/admin/projects');
    } catch (err) {
      toastStore.error(errorMessage(err));
      deleting = false;
    }
  }
</script>

<AdminShell>
  <div class="head">
    <h1 class="page-title">{t('settings.title')}</h1>
    {#if app}
      <p class="muted sub">
        <span class="app-badge"><Icon name={appTypeIcon(app.app_type)} size={15} /> {app.name}</span>
        · {appTypeLabel(app.app_type)}
      </p>
    {/if}
  </div>

  {#if loading}
    <Skeleton rows={6} />
  {:else if error}
    <Card><p class="err-msg">{error}</p></Card>
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
            {app.ingest_enabled
              ? 'This app is accepting events. Disable to stop ingesting without deleting the app.'
              : 'Ingest is paused. Enable to resume accepting events.'}
          </p>
        <Button
          variant={app.ingest_enabled ? 'secondary' : 'primary'}
          loading={togglingIngest}
          lockedReason={updateLock}
          onclick={toggleIngest}
        >
          {app.ingest_enabled ? 'Disable ingest' : 'Enable ingest'}
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
          {#if confirmDelete}
            <div class="confirm">
              <span class="confirm-text">{t('common.delete')} <strong>{app.name}</strong> {t('prose.settings.confirmDelete')}</span>
              <div class="confirm-actions">
                <Button variant="danger" loading={deleting} lockedReason={deleteLock} onclick={doDelete}>{t('projects.confirmDelete')}</Button>
                <Button variant="ghost" onclick={() => (confirmDelete = false)}>{t('common.cancel')}</Button>
              </div>
            </div>
          {:else}
            <Button
              variant="danger"
              lockedReason={deleteLock}
              onclick={() => (confirmDelete = true)}
            >
              {t('settings.deleteApp')}
            </Button>
          {/if}
      </Card>
    </div>
  {/if}
</AdminShell>

<style>
  .head {
    margin-bottom: 20px;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 4px;
  }
  .app-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
    color: var(--text);
  }
  .settings-stack {
    display: flex;
    flex-direction: column;
    gap: 18px;
    max-width: 760px;
  }
  .card-desc {
    font-size: 13px;
    margin-bottom: 14px;
    line-height: 1.55;
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px;
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    border-radius: var(--radius);
  }
  .confirm-text {
    font-size: 13px;
    color: var(--text);
  }
  .confirm-actions {
    display: flex;
    gap: 8px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }
</style>
