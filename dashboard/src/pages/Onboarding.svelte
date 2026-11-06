<script lang="ts">
  import { t } from '../lib/i18n';
  import { onMount, onDestroy } from 'svelte';
  import { push } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import { createProject } from '../lib/api/projects';
  import { createApp, getFirstEvent } from '../lib/api/apps';
  import { listEnvironments } from '../lib/api/environments';
  import { errorMessage } from '../lib/api/client';
  import { buildDsn, appTypeIcon, APP_TYPES } from '../lib/utils/format';
  import type { App, AppType, AppEnvironment, FirstEventStatus, Project } from '../lib/models';

  let project = $state<Project | null>(null);
  let app = $state<App | null>(null);
  // The DSN now lives on the app's seeded environment, not on the app itself.
  let defaultEnv = $state<AppEnvironment | null>(null);

  let projectName = $state('');
  let creatingProject = $state(false);
  let projectError = $state<string | null>(null);

  let appName = $state('');
  let appType = $state<AppType>('web');
  let creatingApp = $state(false);
  let appError = $state<string | null>(null);

  let firstEvent = $state<FirstEventStatus>({ received: false, errors: false, events: false });
  let pollTimer: ReturnType<typeof setInterval> | undefined;

  // Empty (not a malformed placeholder) until the environment fetch resolves,
  // so the DSN step below can guard on it and show the loading affordance.
  const dsn = $derived(defaultEnv ? buildDsn(defaultEnv.public_key, defaultEnv.id) : '');
  const snippet = $derived(
    `import { Sauron } from '@edraj/sauron-browser';\n\nSauron.init({\n  dsn: '${dsn}',\n});`,
  );

  const step = $derived(!project ? 1 : !app ? 2 : 3);

  // The card's heading and lead line, per step. The project and app names used
  // to sit in an accent-coloured span inside the h1; they are plain text now,
  // since the accent is not for headings.
  const heading = $derived(
    step === 1
      ? t('auth.onboarding.createProject')
      : step === 2
        ? t('auth.onboarding.addAppTo', { project: project?.name ?? '' })
        : t('auth.onboarding.connectApp', { app: app?.name ?? '' }),
  );
  const lead = $derived(
    step === 1
      ? t('auth.onboarding.projectHelp')
      : step === 2
        ? t('auth.onboarding.appHelp')
        : t('auth.onboarding.connectHelp'),
  );
  const stepLabel = $derived(
    step === 1
      ? t('auth.onboarding.step1')
      : step === 2
        ? t('auth.onboarding.step2')
        : t('auth.onboarding.step3'),
  );
  // Presence flags, not counts: the API does an existence check so polling
  // this every few seconds need not scan every partition.
  const arriving = $derived(
    firstEvent.errors && firstEvent.events
      ? t('auth.onboarding.arriving.both')
      : firstEvent.errors
        ? t('auth.onboarding.arriving.errors')
        : t('auth.onboarding.arriving.events'),
  );

  // projects.rs create_project is `authorize_org(PROJECT_CREATE)`; apps.rs
  // create_app is `authorize_project(APP_CREATE)`. The two are separate grants,
  // and step 2 is genuinely reachable without the second one: a member holding
  // only `project:create` clears step 1 and would otherwise hit a raw 403 here
  // surfaced as a bare error string naming no permission.
  const createProjectLock = $derived(lockedBy('project:create', { level: 'org' }));
  const createAppLock = $derived(
    project ? lockedBy('app:create', { project: project.id, level: 'project' }) : 'app:create',
  );

  onMount(async () => {
    await sessionStore.load();
    // This page renders no Topbar, so it has no org switcher and its only exit
    // is signing out — which restores the same stored org and lands right back
    // here. That makes arriving in error unrecoverable, so refuse to stay:
    // anyone with a reachable project anywhere belongs in the app, not in
    // first-run setup. `AppShell` already avoids sending them, and this closes
    // the same door from the other side for a stale bookmark or deep link.
    if (sessionStore.reachableProjectCount > 0) {
      push('/overview');
      return;
    }
    // Revisiting with an existing project/app? Pick it up and skip ahead.
    if (sessionStore.currentProject) project = sessionStore.currentProject;
    if (sessionStore.currentApp) {
      app = sessionStore.currentApp;
      startPolling();
      void loadDefaultEnv(app.id);
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

  async function loadDefaultEnv(appId: string) {
    try {
      const envs = await listEnvironments(appId);
      defaultEnv = envs.find((e) => e.is_default) ?? envs[0] ?? null;
    } catch {
      defaultEnv = null;
    }
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
      await loadDefaultEnv(created.id);
    } catch (err) {
      appError = errorMessage(err);
    } finally {
      creatingApp = false;
    }
  }

  // A button, not a link: signing out is an action. As a link, "open in new
  // tab" skipped the handler and opened /login while still signed in.
  async function signOut() {
    await authStore.logout();
    sessionStore.reset();
    push('/login');
  }
</script>

<!-- The steps live inside AuthLayout's wide card: one form per step, and the
     connect step's DSN, snippet and first-event status as sections of it. The
     step counter and the only exit (sign out) share the footer line. -->
<AuthLayout title={heading} subtitle={lead} width="md">
  {#if step === 1}
    <form class="form" onsubmit={handleCreateProject}>
      {#if projectError}<InlineNotice tone="error">{projectError}</InlineNotice>{/if}
      <Input
        label={t('auth.onboarding.projectName')}
        bind:value={projectName}
        placeholder={t('auth.placeholder.projectName')}
        required
      />
      <Button
        type="submit"
        variant="primary"
        size="lg"
        loading={creatingProject}
        lockedReason={createProjectLock}
        fullWidth
      >
        {t('auth.onboarding.createProjectBtn')}
      </Button>
    </form>
  {:else if step === 2}
    <form class="form" onsubmit={handleCreateApp}>
      {#if appError}<InlineNotice tone="error">{appError}</InlineNotice>{/if}
      <Input
        label={t('auth.onboarding.appName')}
        bind:value={appName}
        placeholder={t('auth.placeholder.appName')}
        required
      />
      <div class="field">
        <span class="lbl" id="ob-app-type">{t('auth.onboarding.appType')}</span>
        <div class="type-grid" role="group" aria-labelledby="ob-app-type">
          {#each APP_TYPES as opt (opt.value)}
            <button
              type="button"
              class="type-opt"
              class:selected={appType === opt.value}
              aria-pressed={appType === opt.value}
              onclick={() => (appType = opt.value as AppType)}
            >
              <Icon name={appTypeIcon(opt.value)} size={18} />
              <span class="t-label">{opt.label}</span>
            </button>
          {/each}
        </div>
      </div>
      <Button
        type="submit"
        variant="primary"
        size="lg"
        loading={creatingApp}
        lockedReason={createAppLock}
        fullWidth
      >
        {t('auth.onboarding.createApp')}
      </Button>
    </form>
  {:else if app}
    <div class="connect">
      <Card title={t('auth.onboarding.yourDsn')}>
        {#if dsn}
          <div class="dsn-row">
            <code class="dsn mono">{dsn}</code>
            <CopyButton value={dsn} />
          </div>
        {:else}
          <div class="dsn-loading">
            <Spinner size={16} />
            <span class="muted">{t('auth.onboarding.settingUp')}</span>
          </div>
        {/if}
      </Card>

      {#if dsn}
        <Card title={t('auth.onboarding.installSnippet')}>
          <CodeBlock code={snippet} language="javascript" />
        </Card>
      {/if}

      {#if firstEvent.received}
        <InlineNotice tone="success">
          <strong>{t('auth.onboarding.received')}</strong>
          <span class="muted">{arriving}</span>
          {#snippet action()}
            <Button variant="primary" size="sm" onclick={() => push('/issues')}>
              <span class="btn-inline">
                {t('auth.onboarding.goTo', { page: t('nav.issues') })}
                <Icon name="arrow-right" size={14} />
              </span>
            </Button>
          {/snippet}
        </InlineNotice>
      {:else}
        <div class="waiting">
          <Spinner size={16} />
          <div class="w-text">
            <strong>{t('auth.onboarding.waiting')}</strong>
            <span class="muted">{t('auth.onboarding.polling')}</span>
          </div>
          <Button variant="ghost" size="sm" onclick={() => push('/issues')}>
            {t('auth.onboarding.skip')}
          </Button>
        </div>
      {/if}
    </div>
  {/if}

  {#snippet footer()}
    <span>{stepLabel}</span>
    <span class="sep" aria-hidden="true">·</span>
    <Button variant="ghost" size="sm" onclick={signOut}>{t('auth.signOut')}</Button>
  {/snippet}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
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
    background: none;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text-muted);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease,
      background-color 0.12s ease;
  }
  .type-opt:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .type-opt.selected {
    border-color: var(--primary);
    background: var(--primary-soft);
    color: var(--text);
  }
  .t-label {
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
  }
  .connect {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .dsn-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .dsn {
    flex: 1;
    min-width: 0;
    padding: 8px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: var(--text-2xs);
    overflow-x: auto;
    white-space: nowrap;
    color: var(--text);
  }
  .dsn-loading {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .muted {
    color: var(--text-muted);
  }
  .btn-inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .waiting {
    display: flex;
    align-items: center;
    gap: 12px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .w-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .w-text strong {
    font-weight: var(--weight-medium);
  }
  .w-text .muted {
    font-size: var(--text-xs);
  }
  /* Matches the separator on Login's two-link footer. */
  .sep {
    margin: 0 6px;
    color: var(--text-muted);
  }

  @media (max-width: 480px) {
    .type-grid {
      grid-template-columns: repeat(2, 1fr);
    }
  }
</style>
