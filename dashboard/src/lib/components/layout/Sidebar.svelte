<script lang="ts">
  import { untrack } from 'svelte';
  import { location } from 'svelte-spa-router';
  import EyeMark from '../EyeMark.svelte';
  import Icon, { type IconName } from '../ui/Icon.svelte';
  import { canAccessPage, resolvePageAccess } from '../../models/page-access';
  import { visibleAdminNav } from '../../models/admin-nav';
  import { navCollapseStore } from '../../stores/nav-collapse.svelte';

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
      ],
    },
    {
      label: 'Explore',
      items: [
        { href: '#/events', label: 'Events', icon: 'diamond', match: (p) => p.startsWith('/events') },
        { href: '#/transactions', label: 'Transactions', icon: 'timer', match: (p) => p.startsWith('/transactions') },
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
      label: 'Admin',
      items: [
        { href: '#/admin', label: 'Admin', icon: 'shield-check', match: (p) => p.startsWith('/admin') },
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
        items: g.items.filter((i) => {
          if (!canAccessPage(resolvePageAccess(i.href.slice(1)))) return false;
          // '/admin' is deliberately PAGE_ACCESS: null — no single permission
          // expresses "can reach at least one admin child", and a deep link
          // should still explain itself rather than 404. That makes
          // canAccessPage(null) unconditionally true, so the generic check
          // above can't gate THIS item the way it gates every other one —
          // without its own rule it would violate the invariant this file
          // states above (no item shown that can only ever render an error)
          // for every member on a custom role missing all nine admin-child
          // permissions. `visibleAdminNav()` is the same helper AdminShell's
          // sub-nav rail already uses, so the sidebar item and the rail
          // cannot disagree about whether there is anywhere for it to go.
          if (i.href === '#/admin') return visibleAdminNav().length > 0;
          return true;
        }),
      }))
      .filter((g) => g.items.length > 0),
  );

  const groupId = (label: string) => `nav-group-${label.toLowerCase().replace(/\s+/g, '-')}`;

  // Navigating into a collapsed group opens it, so a route change can never
  // land you on a page whose nav entry is hidden.
  //
  // Deliberately reacting to the ROUTE, not to "is the active group collapsed".
  // The store read is untracked because `expand()` writes the same state this
  // effect would otherwise depend on: with a live dependency, collapsing the
  // group you are currently in would re-run this effect and immediately
  // re-expand it — the toggle would look broken. Untracked, the effect only
  // fires when `$location` actually changes, so a manual collapse sticks.
  $effect(() => {
    const path = $location;
    untrack(() => {
      const owner = visibleGroups.find((g) => g.items.some((i) => i.match(path)));
      if (owner) navCollapseStore.expand(owner.label);
    });
  });
</script>

<aside class="sidebar">
  <a class="brand" href="#/overview">
    <EyeMark size={28} />
    <span class="wordmark">Sauron</span>
  </a>

  <nav class="nav">
    {#each visibleGroups as group (group.label)}
      {@const collapsed = navCollapseStore.isCollapsed(group.label)}
      <div class="group" class:collapsed>
        <button
          class="group-label"
          type="button"
          aria-expanded={!collapsed}
          aria-controls={groupId(group.label)}
          onclick={() => navCollapseStore.toggle(group.label)}
        >
          <span class="chev" aria-hidden="true"><Icon name="chevron-down" size={12} /></span>
          <span class="gl-text">{group.label}</span>
        </button>
        <!-- Items stay in the DOM when collapsed and are hidden with CSS, so
             the ≤860px rule below can override it. Rendering them behind an
             `{#if}` would put the decision in JS, which does not know the
             breakpoint — a group collapsed on desktop would then have its
             items hidden on the mobile rail, where this toggle is
             `display: none` and there is no way to bring them back. -->
        <div class="items" id={groupId(group.label)}>
          {#each group.items as item (item.href)}
            <a class="nav-item" class:active={item.match($location)} href={item.href}>
              <span class="ic"><Icon name={item.icon} size={17} /></span>
              <span class="lb">{item.label}</span>
            </a>
          {/each}
        </div>
      </div>
    {/each}
  </nav>

  <div class="bottom">
    <a class="nav-item" class:active={$location.startsWith('/account')} href="#/account">
      <span class="ic"><Icon name="user" size={17} /></span>
      <span class="lb">Account</span>
    </a>
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
    /* The window is the scroll container (`.shell` is `min-height: 100vh`), so
       without this the nav scrolls off the top of a long page.
       `align-self: start` is load-bearing: as a grid item spanning both rows
       this element otherwise stretches to the full row height, is never
       shorter than its containing block, and `position: sticky` silently does
       nothing. `height: 100vh` then also gives `.bottom`'s `margin-top: auto`
       a definite height to push against, and makes `overflow-y` engage once
       the nav is taller than the viewport. */
    position: sticky;
    top: 0;
    align-self: start;
    height: 100vh;
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
  .items {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .group.collapsed .items {
    display: none;
  }
  .group-label {
    display: flex;
    align-items: center;
    gap: 5px;
    width: 100%;
    background: none;
    border: 0;
    text-align: left;
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-faint);
    padding: 2px 11px 5px;
    border-radius: var(--radius);
    transition: color 0.13s ease;
  }
  .group-label:hover {
    color: var(--text-muted);
  }
  .chev {
    display: grid;
    place-items: center;
    margin-left: -3px;
    transition: transform 0.15s ease;
  }
  .group.collapsed .chev {
    transform: rotate(-90deg);
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
      /* Here the sidebar is a horizontal rail in the FIRST grid row. Left
         sticky at 100vh it would eat the viewport and collide with the
         already-sticky topbar. */
      position: static;
      height: auto;
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
    .items {
      flex-direction: row;
    }
    /* The toggle is hidden on the rail, so a group collapsed on desktop must
       not stay hidden here — there would be no control left to reopen it. */
    .group.collapsed .items {
      display: flex;
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
