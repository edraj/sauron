<script lang="ts">
  import { t } from '../../i18n';
  import { push } from 'svelte-spa-router';
  import { authStore } from '../../stores/auth.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { lockedBy } from '../../models/page-access';
  import { themeStore } from '../../stores/theme.svelte';
  import { initials, appTypeIcon } from '../../utils/format';
  import Icon from '../ui/Icon.svelte';
  import SwitcherMenu from './SwitcherMenu.svelte';

  async function logout() {
    await authStore.logout();
    sessionStore.reset();
    push('/login');
  }

  // Menu items for each breadcrumb segment.
  const orgItems = $derived(sessionStore.orgs.map((o) => ({ id: o.id, name: o.name })));
  const projectItems = $derived(sessionStore.projects.map((p) => ({ id: p.id, name: p.name })));
  const appItems = $derived(
    sessionStore.apps.map((a) => ({ id: a.id, name: a.name, icon: appTypeIcon(a.app_type) })),
  );
  // `sessionStore.environments` is already reach-filtered server-side (Task 7
  // of the env-RBAC slice; see `loadAppEnvironments`'s doc comment in
  // session.svelte.ts) — for a partial-reach member it is only the
  // environments they hold a grant on, not the app's full list. This mapping
  // must NOT grow a client-side filter on top of that: the backend has
  // already applied the only filtering rule that matters, and a second,
  // independent one here would just be a second place for that rule to drift.
  //
  // `''` means "all environments" and `'none'` means "unattributed" — both
  // pseudo-entries bracket the live list. `currentEnvId` is `null` for "all",
  // so the trigger's `currentId` below maps that back to `''` to match.
  //
  // The "All environments" label is intentionally NOT reworded to something
  // like "All my environments" for a partial-reach member, even though
  // selecting it resolves server-side to a `Subset` rather than a literal
  // `All` (`resolve_env_filter`'s row 2). The list directly above this entry
  // in the same dropdown is already exactly that caller's reach-filtered set
  // — there is nothing else "all" could plausibly mean in context, the same
  // way an "All projects" list elsewhere in this app doesn't get relabeled
  // for a member who can only see some of them. Rewording would only be
  // warranted if this entry could show environments beyond what is listed
  // right below it, which by construction it cannot.
  const envItems = $derived([
    { id: '', name: t('nav.allEnvironments') },
    ...sessionStore.environments.map((e) => ({ id: e.id, name: e.name })),
    { id: 'none', name: t('nav.unattributed') },
  ]);

  // The current app's icon (falls back to a generic glyph before apps resolve).
  const currentAppIcon = $derived(appTypeIcon(sessionStore.currentApp?.app_type ?? ''));

  // `removeApp` clears `environments` synchronously without reloading the
  // replacement app's list (see session.svelte.ts), and `setApp`'s same-id
  // no-op guard means `setApp(currentAppId)` can't force one either — so an
  // app selected with no environments loaded yet needs an explicit nudge.
  $effect(() => {
    if (sessionStore.currentAppId && sessionStore.environments.length === 0) {
      void sessionStore.ensureEnvironmentsLoaded();
    }
  });

  // "+ New …" affordances mirror the Projects page, where creation actually
  // happens. Levels match the endpoints: projects.rs:102 authorizes creation at
  // the org, projects.rs:239 authorizes app creation at the project — neither
  // can be satisfied by a grant narrower than that.
  const createProjectLock = $derived(lockedBy('project:create', { level: 'org' }));
  const createAppLock = $derived(
    lockedBy('app:create', { project: sessionStore.currentProjectId, level: 'project' }),
  );
</script>

<header class="topbar">
  <div class="left">
    <!-- Org switcher -->
    {#if orgItems.length > 0}
      <SwitcherMenu
        label={t('nav.org')}
        items={orgItems}
        currentId={sessionStore.currentOrgId}
        onSelect={(id) => void sessionStore.setOrg(id)}
        ariaLabel={t('nav.switchOrg')}
      />
    {/if}

    <!-- Project switcher -->
    {#if projectItems.length > 0}
      <span class="sep" aria-hidden="true">/</span>
      <div class="project-switcher">
        <SwitcherMenu
          label={t('nav.project')}
          items={projectItems}
          currentId={sessionStore.currentProjectId}
          onSelect={(id) => void sessionStore.setProject(id)}
          createLabel="New project"
          onCreate={() => push('/admin/projects')}
          createLocked={createProjectLock}
          ariaLabel={t('nav.switchProject')}
        />
      </div>
    {/if}

    <!-- App switcher -->
    {#if appItems.length > 0}
      <span class="sep" aria-hidden="true">/</span>
      <SwitcherMenu
        triggerIcon={currentAppIcon}
        items={appItems}
        currentId={sessionStore.currentAppId}
        onSelect={(id) => void sessionStore.setApp(id)}
        createLabel="New app"
        onCreate={() => push('/admin/projects')}
        createLocked={createAppLock}
        ariaLabel={t('nav.switchApp')}
      />
    {/if}

    <!-- Environment switcher — app and environment are what change the
         meaning of the data on screen, so this stays visible (with its name)
         at widths where the project switcher's name gets dropped instead. -->
    {#if sessionStore.currentAppId}
      <span class="sep" aria-hidden="true">/</span>
      <SwitcherMenu
        label={t('nav.env')}
        items={envItems}
        currentId={sessionStore.currentEnvId ?? ''}
        onSelect={(id) => void sessionStore.setEnvironment(id === '' ? null : id)}
        ariaLabel={t('nav.switchEnvironment')}
      />
    {/if}
  </div>

  <div class="right">
    <a class="icon-btn" href="#/docs" title={t('nav.docsTitle')} aria-label={t('nav.docs')}>
      <Icon name="life-buoy" size={16} />
    </a>

    <button
      class="icon-btn"
      title={themeStore.theme === 'dark' ? t('nav.switchToLight') : t('nav.switchToDark')}
      aria-label={t('nav.theme.toggle')}
      onclick={() => themeStore.toggle()}
    >
      <Icon name={themeStore.theme === 'dark' ? 'moon' : 'sun'} size={16} />
    </button>

    <div class="user">
      <span class="avatar" title={authStore.user?.email}>
        {initials(authStore.user?.name || authStore.user?.email || '?')}
      </span>
      <div class="user-meta">
        <span class="u-name">{authStore.user?.name || t('nav.account')}</span>
        <span class="u-email">{authStore.user?.email}</span>
      </div>
    </div>

    <button class="logout" onclick={logout} title={t('nav.logOut')}>{t('nav.logOut')}</button>
  </div>
</header>

<style>
  .topbar {
    grid-area: topbar;
    height: var(--topbar-h);
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 0 22px;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--surface) 55%, var(--bg));
    backdrop-filter: blur(8px);
    position: sticky;
    top: 0;
    z-index: 20;
  }
  .left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    overflow: hidden;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .sep {
    color: var(--text-faint);
    font-size: 13px;
  }
  .icon-btn {
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 16px;
    transition: all 0.13s ease;
  }
  .icon-btn:hover {
    color: var(--text);
    background: var(--surface-3);
  }
  .user {
    display: flex;
    align-items: center;
    gap: 9px;
    padding-inline-start: 4px;
  }
  .avatar {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 12px;
    font-weight: 650;
    flex-shrink: 0;
  }
  .user-meta {
    display: flex;
    flex-direction: column;
    line-height: 1.25;
  }
  .u-name {
    font-size: 13px;
    font-weight: 560;
  }
  .u-email {
    font-size: 11px;
    color: var(--text-faint);
  }
  .logout {
    background: transparent;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text-muted);
    padding: 7px 12px;
    font-size: 12.5px;
    font-weight: 520;
    transition: all 0.13s ease;
  }
  .logout:hover {
    color: var(--text);
    border-color: var(--text-faint);
  }

  @media (max-width: 640px) {
    .user-meta {
      display: none;
    }
    .topbar {
      padding: 0 14px;
    }
    /* App and environment are what change the meaning of the data on
       screen; project is navigational. When the four triggers no longer
       fit, drop the project switcher's name first (its "Project" label
       chip is already gone below 860px via SwitcherMenu's own rule), not
       the app's or environment's. */
    .project-switcher :global(.name) {
      display: none;
    }
  }
</style>
