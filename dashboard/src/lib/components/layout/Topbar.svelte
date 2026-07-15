<script lang="ts">
  import { push } from 'svelte-spa-router';
  import { authStore } from '../../stores/auth.svelte';
  import { sessionStore } from '../../stores/session.svelte';
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

  // The current app's icon (falls back to a generic glyph before apps resolve).
  const currentAppIcon = $derived(appTypeIcon(sessionStore.currentApp?.app_type ?? ''));

  // "+ New …" affordances mirror the Projects page, where creation actually happens.
  const canCreateProject = $derived(sessionStore.can('project:create'));
  const canCreateApp = $derived(sessionStore.can('app:create'));
</script>

<header class="topbar">
  <div class="left">
    <!-- Org switcher -->
    {#if orgItems.length > 0}
      <SwitcherMenu
        label="Org"
        items={orgItems}
        currentId={sessionStore.currentOrgId}
        onSelect={(id) => void sessionStore.setOrg(id)}
        ariaLabel="Switch organization"
      />
    {/if}

    <!-- Project switcher -->
    {#if projectItems.length > 0}
      <span class="sep" aria-hidden="true">/</span>
      <SwitcherMenu
        label="Project"
        items={projectItems}
        currentId={sessionStore.currentProjectId}
        onSelect={(id) => void sessionStore.setProject(id)}
        createLabel={canCreateProject ? 'New project' : undefined}
        onCreate={canCreateProject ? () => push('/projects') : undefined}
        ariaLabel="Switch project"
      />
    {/if}

    <!-- App switcher -->
    {#if appItems.length > 0}
      <span class="sep" aria-hidden="true">/</span>
      <SwitcherMenu
        triggerIcon={currentAppIcon}
        items={appItems}
        currentId={sessionStore.currentAppId}
        onSelect={(id) => sessionStore.setApp(id)}
        createLabel={canCreateApp ? 'New app' : undefined}
        onCreate={canCreateApp ? () => push('/projects') : undefined}
        ariaLabel="Switch app"
      />
    {/if}
  </div>

  <div class="right">
    <a class="icon-btn" href="#/docs" title="Docs & integration guides" aria-label="Docs">
      <Icon name="life-buoy" size={16} />
    </a>

    <button
      class="icon-btn"
      title={themeStore.theme === 'dark' ? 'Switch to light' : 'Switch to dark'}
      aria-label="Toggle theme"
      onclick={() => themeStore.toggle()}
    >
      <Icon name={themeStore.theme === 'dark' ? 'moon' : 'sun'} size={16} />
    </button>

    <div class="user">
      <span class="avatar" title={authStore.user?.email}>
        {initials(authStore.user?.name || authStore.user?.email || '?')}
      </span>
      <div class="user-meta">
        <span class="u-name">{authStore.user?.name || 'Account'}</span>
        <span class="u-email">{authStore.user?.email}</span>
      </div>
    </div>

    <button class="logout" onclick={logout} title="Log out">Log out</button>
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
    padding-left: 4px;
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
  }
</style>
