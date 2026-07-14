<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { push } from 'svelte-spa-router';
  import Input from '../lib/components/ui/Input.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { createProject } from '../lib/api/projects';
  import { createApp, getFirstEvent } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { buildDsn, appTypeIcon, APP_TYPES } from '../lib/utils/format';
  import type { App, AppType, FirstEventStatus, Project } from '../lib/models';

  let project = $state<Project | null>(null);
  let app = $state<App | null>(null);

  let projectName = $state('');
  let creatingProject = $state(false);
  let projectError = $state<string | null>(null);

  let appName = $state('');
  let appType = $state<AppType>('web');
  let creatingApp = $state(false);
  let appError = $state<string | null>(null);

  let firstEvent = $state<FirstEventStatus>({ received: false, errors: 0, events: 0 });
  let pollTimer: ReturnType<typeof setInterval> | undefined;

  const dsn = $derived(app ? buildDsn(app.public_key, app.id) : '');
  const snippet = $derived(
    `import { Sauron } from '@sauron/browser';\n\nSauron.init({\n  dsn: '${dsn}',\n});`,
  );

  const step = $derived(!project ? 1 : !app ? 2 : 3);

  onMount(async () => {
    await sessionStore.load();
    // Revisiting with an existing project/app? Pick it up and skip ahead.
    if (sessionStore.currentProject) project = sessionStore.currentProject;
    if (sessionStore.currentApp) {
      app = sessionStore.currentApp;
      startPolling();
    }
  });

  onDestroy(() => stopPolling());

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = undefined;
    }
  }

  async function pollOnce() {
    if (!app) return;
    try {
      firstEvent = await getFirstEvent(app.id);
      if (firstEvent.received) stopPolling();
    } catch {
      /* transient — keep polling */
    }
  }

  function startPolling() {
    stopPolling();
    void pollOnce();
    pollTimer = setInterval(pollOnce, 3000);
  }

  async function handleCreateProject(event: SubmitEvent) {
    event.preventDefault();
    if (creatingProject || !sessionStore.currentOrgId || !projectName.trim()) return;
    projectError = null;
    creatingProject = true;
    try {
      const created = await createProject(sessionStore.currentOrgId, { name: projectName.trim() });
      sessionStore.upsertProject(created);
      project = created;
    } catch (err) {
      projectError = errorMessage(err);
    } finally {
      creatingProject = false;
    }
  }

  async function handleCreateApp(event: SubmitEvent) {
    event.preventDefault();
    if (creatingApp || !project || !appName.trim()) return;
    appError = null;
    creatingApp = true;
    try {
      const created = await createApp(project.id, { name: appName.trim(), app_type: appType });
      sessionStore.upsertApp(created);
      app = created;
      startPolling();
    } catch (err) {
      appError = errorMessage(err);
    } finally {
      creatingApp = false;
    }
  }

  async function signOut() {
    await authStore.logout();
    sessionStore.reset();
    push('/login');
  }
</script>

<div class="ob">
  <header class="ob-top">
    <div class="brand">
      <span class="mark" aria-hidden="true"><span class="eye"></span></span>
      <span class="name">Sauron</span>
    </div>
    <button class="link" onclick={signOut}>Sign out</button>
  </header>

  <div class="ob-body">
    {#if step === 1}
      <div class="intro">
        <span class="step-pill">Step 1 of 3</span>
        <h1>Create your first project</h1>
        <p class="lead">
          A project groups related apps — think a product or a team. You'll add an app next.
        </p>
      </div>
      <Card>
        <form class="create-form" onsubmit={handleCreateProject}>
          {#if projectError}<div class="alert">{projectError}</div>{/if}
          <Input label="Project name" bind:value={projectName} placeholder="Payments" required />
          <Button type="submit" variant="primary" size="lg" loading={creatingProject} fullWidth>
            Create project
          </Button>
        </form>
      </Card>
    {:else if step === 2}
      <div class="intro">
        <span class="step-pill">Step 2 of 3</span>
        <h1>Add an app to <span class="hl">{project?.name}</span></h1>
        <p class="lead">An app holds the DSN your SDK reports to. Pick the platform it runs on.</p>
      </div>
      <Card>
        <form class="create-form" onsubmit={handleCreateApp}>
          {#if appError}<div class="alert">{appError}</div>{/if}
          <Input label="App name" bind:value={appName} placeholder="Web App" required />
          <div class="field">
            <span class="lbl">App type</span>
            <div class="type-grid">
              {#each APP_TYPES as t (t.value)}
                <button
                  type="button"
                  class="type-opt"
                  class:selected={appType === t.value}
                  onclick={() => (appType = t.value as AppType)}
                >
                  <span class="t-icon"><Icon name={appTypeIcon(t.value)} size={18} /></span>
                  <span class="t-label">{t.label}</span>
                </button>
              {/each}
            </div>
          </div>
          <Button type="submit" variant="primary" size="lg" loading={creatingApp} fullWidth>
            Create app
          </Button>
        </form>
      </Card>
    {:else if app}
      <div class="intro">
        <span class="step-pill">Step 3 of 3</span>
        <h1>Connect <span class="hl"><Icon name={appTypeIcon(app.app_type)} size={18} /> {app.name}</span></h1>
        <p class="lead">
          Initialize the SDK with your DSN. We'll light up as soon as the first event arrives.
        </p>
      </div>

      <Card title="Your DSN">
        <div class="dsn-row">
          <code class="dsn mono">{dsn}</code>
          <CopyButton value={dsn} />
        </div>
      </Card>

      <Card title="Install snippet">
        <CodeBlock code={snippet} language="javascript" />
      </Card>

      <div class="waiting" class:done={firstEvent.received}>
        {#if firstEvent.received}
          <div class="w-icon done-icon"><Icon name="check" size={16} /></div>
          <div class="w-text">
            <strong>First event received!</strong>
            <span class="muted">
              {firstEvent.errors} error{firstEvent.errors === 1 ? '' : 's'} ·
              {firstEvent.events} event{firstEvent.events === 1 ? '' : 's'} ingested.
            </span>
          </div>
          <Button variant="primary" onclick={() => push('/issues')}>
            <span class="btn-inline">Go to Issues <Icon name="arrow-right" size={14} /></span>
          </Button>
        {:else}
          <div class="w-icon"><Spinner size={18} /></div>
          <div class="w-text">
            <strong>Waiting for your first event…</strong>
            <span class="muted">Send an error or event from your app. Polling every 3s.</span>
          </div>
          <button class="link" onclick={() => push('/issues')}>Skip for now</button>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .ob {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .ob-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 24px;
    border-bottom: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .mark {
    width: 28px;
    height: 28px;
    border-radius: 8px;
    background: radial-gradient(circle at 50% 45%, #ffe08a 0%, #f5a623 45%, #e0524a 100%);
    display: grid;
    place-items: center;
  }
  .eye {
    width: 5px;
    height: 15px;
    background: #0a0c10;
    border-radius: 50%;
  }
  .name {
    font-weight: 700;
    font-size: 16px;
  }
  .link {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
  }
  .link:hover {
    color: var(--text);
    text-decoration: underline;
  }
  .ob-body {
    flex: 1;
    width: 100%;
    max-width: 620px;
    margin: 0 auto;
    padding: 40px 22px 64px;
    display: flex;
    flex-direction: column;
    gap: 18px;
    animation: fade-in 0.25s ease;
  }
  .intro {
    margin-bottom: 4px;
  }
  .step-pill {
    display: inline-block;
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--primary);
    background: var(--primary-soft);
    padding: 4px 10px;
    border-radius: var(--radius-pill);
    margin-bottom: 12px;
  }
  .intro h1 {
    font-size: 25px;
  }
  .hl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--primary);
  }
  .btn-inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .lead {
    color: var(--text-muted);
    margin-top: 8px;
    font-size: 14px;
  }
  .create-form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .lbl {
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
  }
  .type-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }
  .type-opt {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 12px 8px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text-muted);
    transition: all 0.13s ease;
  }
  .type-opt:hover {
    border-color: var(--text-faint);
    color: var(--text);
  }
  .type-opt.selected {
    border-color: var(--primary);
    background: var(--primary-soft);
    color: var(--primary);
  }
  .t-icon {
    font-size: 20px;
    line-height: 1;
  }
  .t-label {
    font-size: 12px;
    font-weight: 540;
  }
  .alert {
    padding: 10px 12px;
    border-radius: var(--radius);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    color: var(--error);
    font-size: 13px;
  }
  .dsn-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .dsn {
    flex: 1;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12.5px;
    overflow-x: auto;
    white-space: nowrap;
    color: var(--text);
  }
  .waiting {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 16px 18px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }
  .waiting.done {
    border-color: color-mix(in srgb, var(--success) 45%, transparent);
    background: color-mix(in srgb, var(--success-soft) 60%, var(--surface));
  }
  .w-icon {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--surface-2);
    flex-shrink: 0;
  }
  .done-icon {
    background: var(--success);
    color: #fff;
    font-weight: 700;
  }
  .w-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
  }
  .w-text strong {
    font-size: 14px;
    font-weight: 620;
  }
  .w-text .muted {
    font-size: 12.5px;
  }

  @media (max-width: 480px) {
    .type-grid {
      grid-template-columns: repeat(2, 1fr);
    }
  }
</style>
