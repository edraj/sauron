<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import {
    getApp,
    rotateAppKey,
    updateApp,
    deleteApp,
    listEnvironments,
  } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    relativeTime,
    formatDateTime,
    buildDsn,
    appTypeIcon,
    appTypeLabel,
  } from '../lib/utils/format';
  import type { App, Environment } from '../lib/models';

  let app = $state<App | null>(null);
  let environments = $state<Environment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let rotating = $state(false);
  let confirmRotate = $state(false);
  let togglingIngest = $state(false);
  let confirmDelete = $state(false);
  let deleting = $state(false);

  const canUpdate = $derived(app ? sessionStore.can('app:update', { app: app.id }) : false);
  const canRotate = $derived(app ? sessionStore.can('app:rotate_key', { app: app.id }) : false);
  const canDelete = $derived(app ? sessionStore.can('app:delete', { app: app.id }) : false);

  const dsn = $derived(app ? buildDsn(app.public_key, app.id) : '');
  const snippet = $derived(
    `import { Sauron } from '@sauron/browser';\n\nSauron.init({\n  dsn: '${dsn}',\n});`,
  );

  async function load(appId: string) {
    loading = true;
    error = null;
    try {
      const [a, envs] = await Promise.all([
        getApp(appId),
        listEnvironments(appId).catch(() => [] as Environment[]),
      ]);
      app = a;
      environments = envs;
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

  async function doRotate() {
    if (!app || rotating) return;
    rotating = true;
    try {
      const updated = await rotateAppKey(app.id);
      app = updated;
      sessionStore.upsertApp(updated, false);
      confirmRotate = false;
      toastStore.success('Public key regenerated. Update your DSN everywhere.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      rotating = false;
    }
  }

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
      <Card title="Client DSN">
        <p class="card-desc muted">
          Point your SDK at this DSN to start ingesting. The public key is safe to embed in
          client-side code.
        </p>
        <div class="dsn-row">
          <code class="dsn mono">{dsn}</code>
          <CopyButton value={dsn} />
        </div>
        <div class="key-line">
          <span class="section-label">Public key</span>
          <code class="mono pk">{app.public_key}</code>
          <Badge tone={app.ingest_enabled ? 'success' : 'neutral'} size="sm" dot>
            {app.ingest_enabled ? 'ingest enabled' : 'ingest disabled'}
          </Badge>
        </div>
      </Card>

      <Card title="Install snippet">
        <CodeBlock code={snippet} language="javascript" />
      </Card>

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

      {#if canRotate}
        <Card title="Regenerate key">
          <p class="card-desc muted">
            Rotating the public key immediately invalidates the old one. Any client still using
            the previous DSN will stop reporting until updated.
          </p>
          {#if confirmRotate}
            <div class="confirm">
              <span class="confirm-text">This can't be undone. Regenerate the public key?</span>
              <div class="confirm-actions">
                <Button variant="danger" loading={rotating} onclick={doRotate}>
                  Yes, regenerate
                </Button>
                <Button variant="ghost" onclick={() => (confirmRotate = false)}>Cancel</Button>
              </div>
            </div>
          {:else}
            <Button variant="secondary" onclick={() => (confirmRotate = true)}>
              Regenerate public key
            </Button>
          {/if}
        </Card>
      {/if}

      <Card title="Environments">
        {#if environments.length === 0}
          <p class="muted">No environments yet — they're created automatically on first ingest.</p>
        {:else}
          <ul class="env-list">
            {#each environments as env (env.id)}
              <li class="env">
                <span class="env-dot"></span>
                <span class="env-name">{env.name}</span>
                <span class="env-time muted" title={formatDateTime(env.created_at)}>
                  created {relativeTime(env.created_at)}
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>

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
  .dsn-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .dsn {
    flex: 1;
    padding: 11px 13px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12.5px;
    overflow-x: auto;
    white-space: nowrap;
    color: var(--text);
  }
  .key-line {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 16px;
    flex-wrap: wrap;
  }
  .pk {
    font-size: 12.5px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 4px 9px;
    border-radius: var(--radius-sm);
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
  .env-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .env {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 8px;
    border-bottom: 1px solid var(--border);
  }
  .env:last-child {
    border-bottom: none;
  }
  .env-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--success);
    flex-shrink: 0;
  }
  .env-name {
    font-weight: 560;
    font-size: 13.5px;
  }
  .env-time {
    margin-left: auto;
    font-size: 12px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }
</style>
