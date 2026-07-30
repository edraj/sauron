<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import EnvironmentsCard from '../lib/components/settings/EnvironmentsCard.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getApp, updateApp, deleteApp } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { appTypeIcon, appTypeLabel } from '../lib/utils/format';
  import type { App } from '../lib/models';

  let app = $state<App | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let togglingIngest = $state(false);
  let confirmDelete = $state(false);
  let deleting = $state(false);

  const canUpdate = $derived(app ? sessionStore.can('app:update', { app: app.id }) : false);
  const canDelete = $derived(app ? sessionStore.can('app:delete', { app: app.id }) : false);

  async function load(appId: string) {
    loading = true;
    error = null;
    try {
      app = await getApp(appId);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid);
  });

  async function toggleIngest() {
    if (!app || togglingIngest) return;
    togglingIngest = true;
    const next = !app.ingest_enabled;
    try {
      const updated = await updateApp(app.id, { ingest_enabled: next });
      app = updated;
      sessionStore.upsertApp(updated, false);
      toastStore.success(next ? 'Ingest enabled.' : 'Ingest disabled.');
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
      push('/projects');
    } catch (err) {
      toastStore.error(errorMessage(err));
      deleting = false;
    }
  }
</script>

<AppShell requireProject={false}>
  <div class="head">
    <h1 class="page-title">App settings</h1>
    {#if app}
      <p class="muted sub">
        <span class="app-badge"><Icon name={appTypeIcon(app.app_type)} size={15} /> {app.name}</span>
        · {appTypeLabel(app.app_type)}
      </p>
    {/if}
  </div>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <Card><p class="err-msg">{error}</p></Card>
  {:else if !app}
    <EmptyState
      title="No app selected"
      description="Pick an app from the switcher, or create one from Projects."
      icon="package"
    >
      {#snippet action()}
        <Button variant="primary" onclick={() => push('/projects')}>Go to Projects</Button>
      {/snippet}
    </EmptyState>
  {:else}
    <div class="settings-stack">
      {#if canUpdate}
        <Card title="Ingest">
          <p class="card-desc muted">
            {app.ingest_enabled
              ? 'This app is accepting events. Disable to stop ingesting without deleting the app.'
              : 'Ingest is paused. Enable to resume accepting events.'}
          </p>
          <Button
            variant={app.ingest_enabled ? 'secondary' : 'primary'}
            loading={togglingIngest}
            onclick={toggleIngest}
          >
            {app.ingest_enabled ? 'Disable ingest' : 'Enable ingest'}
          </Button>
        </Card>
      {/if}

      <!-- Environments are owned by the project now, so the card needs the app's
           project id as well: creating one is a catalogue write under
           `/v1/projects/{project_id}/environments`, not an app write. -->
      <EnvironmentsCard appId={app.id} projectId={app.project_id} />

      {#if canDelete}
        <Card title="Delete app">
          <p class="card-desc muted">
            Permanently delete this app and all of its issues and events. This can't be undone.
          </p>
          {#if confirmDelete}
            <div class="confirm">
              <span class="confirm-text">Delete <strong>{app.name}</strong> and all its data?</span>
              <div class="confirm-actions">
                <Button variant="danger" loading={deleting} onclick={doDelete}>Yes, delete</Button>
                <Button variant="ghost" onclick={() => (confirmDelete = false)}>Cancel</Button>
              </div>
            </div>
          {:else}
            <Button variant="danger" onclick={() => (confirmDelete = true)}>Delete app</Button>
          {/if}
        </Card>
      {/if}
    </div>
  {/if}
</AppShell>

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
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
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
