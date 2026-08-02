<script lang="ts">
  import { location } from 'svelte-spa-router';
  import EyeMark from '../EyeMark.svelte';
  import Icon, { type IconName } from '../ui/Icon.svelte';
  import { canAccessPage, resolvePageAccess } from '../../models/page-access';

  interface NavItem {
    href: string;
    label: string;
    icon: IconName;
    match: (path: string) => boolean;
  }

  interface NavGroup {
    label: string;
    items: NavItem[];
  }

  const groups: NavGroup[] = [
    {
      label: 'Monitor',
      items: [
        { href: '#/overview', label: 'Overview', icon: 'layout-dashboard', match: (p) => p.startsWith('/overview') },
        { href: '#/issues', label: 'Exceptions', icon: 'triangle-alert', match: (p) => p.startsWith('/issues') },
        { href: '#/performance', label: 'Performance', icon: 'zap', match: (p) => p.startsWith('/performance') },
      ],
    },
    {
      label: 'Uptime',
      items: [
        { href: '#/monitors', label: 'Monitors', icon: 'life-buoy', match: (p) => p.startsWith('/monitors') },
        { href: '#/alerts', label: 'Alerts', icon: 'bell', match: (p) => p.startsWith('/alerts') },
      ],
    },
    {
      label: 'Explore',
      items: [
        { href: '#/events', label: 'Events', icon: 'diamond', match: (p) => p.startsWith('/events') },
        { href: '#/sessions', label: 'Sessions', icon: 'clock', match: (p) => p.startsWith('/sessions') },
        { href: '#/users', label: 'Users', icon: 'users', match: (p) => p.startsWith('/users') || p.startsWith('/persons') },
        { href: '#/devices', label: 'Devices', icon: 'monitor-smartphone', match: (p) => p.startsWith('/devices') },
        { href: '#/screens', label: 'Screens', icon: 'layout-panel-top', match: (p) => p.startsWith('/screens') },
        { href: '#/workflows', label: 'Workflows', icon: 'workflow', match: (p) => p.startsWith('/workflows') },
      ],
    },
    {
      label: 'Analyze',
      items: [
        { href: '#/active-users', label: 'Active users', icon: 'users', match: (p) => p.startsWith('/active-users') },
        { href: '#/funnels', label: 'Funnels', icon: 'funnel', match: (p) => p.startsWith('/funnels') },
        { href: '#/journeys', label: 'Journeys', icon: 'waypoints', match: (p) => p.startsWith('/journeys') },
      ],
    },
    {
      label: 'Manage',
      items: [
        { href: '#/account', label: 'Account', icon: 'user', match: (p) => p.startsWith('/account') },
        { href: '#/projects', label: 'Projects', icon: 'folders', match: (p) => p.startsWith('/projects') || p.startsWith('/apps') },
        { href: '#/members', label: 'Members', icon: 'key-round', match: (p) => p.startsWith('/members') },
        { href: '#/settings', label: 'App settings', icon: 'settings', match: (p) => p.startsWith('/settings') },
        { href: '#/source-maps', label: 'Source Maps', icon: 'braces', match: (p) => p.startsWith('/source-maps') },
        { href: '#/storage', label: 'Storage', icon: 'server', match: (p) => p.startsWith('/storage') },
        { href: '#/inspector', label: 'Privacy', icon: 'shield-alert', match: (p) => p.startsWith('/inspector') },
      ],
    },
  ];

  // Visibility comes from PAGE_ACCESS, not a per-item `show` predicate.
  // Predicates only ever got written for the items someone remembered: 13 of
  // the 20 below had none, so a member without `event:read` was shown eleven
  // pages that could only ever render an error. One table also means this list
  // and routes.ts can no longer drift, which page-access.test.ts enforces.
  //
  // `item.href` is '#/issues'; slice(1) yields the '/issues' the table is
  // keyed by.
  const visibleGroups = $derived(
    groups
      .map((g) => ({
        ...g,
        items: g.items.filter((i) => canAccessPage(resolvePageAccess(i.href.slice(1)))),
      }))
      .filter((g) => g.items.length > 0),
  );
</script>

<aside class="sidebar">
  <a class="brand" href="#/overview">
    <EyeMark size={28} />
    <span class="wordmark">Sauron</span>
  </a>

  <nav class="nav">
    {#each visibleGroups as group (group.label)}
      <div class="group">
        <span class="group-label">{group.label}</span>
        {#each group.items as item (item.href)}
          <a class="nav-item" class:active={item.match($location)} href={item.href}>
            <span class="ic"><Icon name={item.icon} size={17} /></span>
            <span class="lb">{item.label}</span>
          </a>
        {/each}
      </div>
    {/each}
  </nav>

  <div class="bottom">
    <a class="nav-item" class:active={$location.startsWith('/docs')} href="#/docs">
      <span class="ic"><Icon name="book-open" size={17} /></span>
      <span class="lb">Docs</span>
    </a>
    <div class="foot">
      <span class="foot-label">Observability &amp; product analytics</span>
    </div>
  </div>
</aside>

<style>
  .sidebar {
    grid-area: sidebar;
    width: var(--sidebar-w);
    display: flex;
    flex-direction: column;
    background: color-mix(in srgb, var(--surface) 60%, var(--bg));
    border-right: 1px solid var(--border);
    padding: 16px 12px;
    overflow-y: auto;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px 16px;
  }
  .wordmark {
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .nav {
    display: flex;
    flex-direction: column;
    gap: 14px;
    margin-top: 4px;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .group-label {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-faint);
    padding: 2px 11px 5px;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 8px 11px;
    border-radius: var(--radius);
    color: var(--text-muted);
    font-weight: 520;
    font-size: 13.5px;
    transition: background 0.13s ease, color 0.13s ease;
  }
  .nav-item:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .nav-item.active {
    background: var(--primary-soft);
    color: var(--primary);
  }
  .ic {
    width: 18px;
    display: grid;
    place-items: center;
    flex-shrink: 0;
  }
  .bottom {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .foot {
    padding: 12px 10px 4px;
  }
  .foot-label {
    font-size: 11px;
    color: var(--text-faint);
    line-height: 1.4;
    display: block;
  }

  @media (max-width: 860px) {
    .sidebar {
      width: 100%;
      flex-direction: row;
      align-items: center;
      padding: 8px 12px;
      border-right: none;
      border-bottom: 1px solid var(--border);
      overflow-x: auto;
      overflow-y: hidden;
    }
    .brand {
      padding: 4px 8px;
    }
    .nav {
      flex-direction: row;
      margin: 0 0 0 10px;
      gap: 10px;
    }
    .group {
      flex-direction: row;
      align-items: center;
      gap: 2px;
    }
    .group-label {
      display: none;
    }
    .nav-item .lb {
      display: none;
    }
    .bottom {
      margin: 0 0 0 6px;
      flex-direction: row;
    }
    .foot {
      display: none;
    }
  }
</style>
