<script lang="ts">
  import { push } from 'svelte-spa-router';
  import { authStore } from '../../stores/auth.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { themeStore } from '../../stores/theme.svelte';
  import { initials, appTypeIcon } from '../../utils/format';
  import Icon from '../ui/Icon.svelte';

  async function logout() {
    await authStore.logout();
    sessionStore.reset();
    push('/login');
  }

  function onOrgChange(event: Event) {
    void sessionStore.setOrg((event.target as HTMLSelectElement).value);
  }
  function onProjectChange(event: Event) {
    void sessionStore.setProject((event.target as HTMLSelectElement).value);
  }
  function onAppChange(event: Event) {
    sessionStore.setApp((event.target as HTMLSelectElement).value);
  }
</script>

<header class="topbar">
  <div class="left">
    <!-- Org switcher (usually a single org → shown as a static label). -->
    {#if sessionStore.orgs.length > 0}
      <div class="switcher">
        <span class="sw-kind">Org</span>
        {#if sessionStore.orgs.length > 1}
          <select aria-label="Select organization" value={sessionStore.currentOrgId} onchange={onOrgChange}>
            {#each sessionStore.orgs as org (org.id)}
              <option value={org.id}>{org.name}</option>
            {/each}
          </select>
        {:else}
          <span class="sw-name">{sessionStore.currentOrg?.name}</span>
        {/if}
      </div>
    {/if}

    <!-- Project switcher -->
    {#if sessionStore.projects.length > 0}
      <span class="sep" aria-hidden="true">/</span>
      <div class="switcher">
        <span class="sw-kind">Project</span>
        {#if sessionStore.projects.length > 1}
          <select aria-label="Select project" value={sessionStore.currentProjectId} onchange={onProjectChange}>
            {#each sessionStore.projects as project (project.id)}
              <option value={project.id}>{project.name}</option>
            {/each}
          </select>
        {:else}
          <span class="sw-name">{sessionStore.currentProject?.name}</span>
        {/if}
      </div>
    {/if}

    <!-- App switcher -->
    {#if sessionStore.apps.length > 0}
      <span class="sep" aria-hidden="true">/</span>
      <div class="switcher app-switcher">
        <span class="app-icon" aria-hidden="true"><Icon name={appTypeIcon(sessionStore.currentApp?.app_type ?? '')} size={15} /></span>
        {#if sessionStore.apps.length > 1}
          <select aria-label="Select app" value={sessionStore.currentAppId} onchange={onAppChange}>
            {#each sessionStore.apps as app (app.id)}
              <option value={app.id}>{app.name}</option>
            {/each}
          </select>
        {:else}
          <span class="sw-name">{sessionStore.currentApp?.name}</span>
        {/if}
      </div>
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
  .switcher {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    min-width: 0;
  }
  .sw-kind {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .app-icon {
    font-size: 13px;
    line-height: 1;
  }
  .sep {
    color: var(--text-faint);
    font-size: 13px;
  }
  .sw-name {
    font-weight: 600;
    font-size: 13.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 180px;
  }
  select {
    background: transparent;
    border: none;
    color: var(--text);
    font-weight: 600;
    font-size: 13.5px;
    outline: none;
    cursor: pointer;
    max-width: 200px;
  }
  select option {
    background: var(--surface);
    color: var(--text);
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

  @media (max-width: 860px) {
    .sw-kind {
      display: none;
    }
    .switcher {
      padding: 6px 10px;
    }
  }
  @media (max-width: 640px) {
    .user-meta {
      display: none;
    }
    .topbar {
      padding: 0 14px;
    }
    .sw-name,
    select {
      max-width: 110px;
    }
  }
</style>
