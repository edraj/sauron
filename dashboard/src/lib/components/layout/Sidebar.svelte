<script lang="ts">
  import { untrack } from 'svelte';
  import { location } from 'svelte-spa-router';
  import EyeMark from '../EyeMark.svelte';
  import Icon, { type IconName } from '../ui/Icon.svelte';
  import { pageLockedBy } from '../../models/page-access';
  import { lockTip } from '../../actions/lock-tip';
  import { navCollapseStore } from '../../stores/nav-collapse.svelte';
  import { t } from '../../i18n';

  interface NavItem {
    href: string;
    label: string;
    icon: IconName;
    match: (path: string) => boolean;
  }

  interface NavGroup {
    /**
     * Stable, language-independent identity.
     *
     * Separate from `label` because the label is now translated, and three
     * things key off the group that must NOT move when the language does: the
     * `{#each}` key, the persisted collapse state in `navCollapseStore`, and
     * the `aria-controls` DOM id. Keying any of them on translated text would
     * reset every user's collapsed groups on a language switch and emit
     * Arabic-script element ids.
     */
    id: string;
    label: string;
    items: NavItem[];
  }

  // `$derived`, not a plain `const`: `t()` reads the locale store, and a value
  // computed once at component init would keep the language the sidebar
  // happened to mount in. Only the display strings move — `id`, `href`, `icon`
  // and `match` are identity and routing, and stay fixed.
  const groups: NavGroup[] = $derived([
    {
      id: 'monitor',
      label: t('nav.group.monitor'),
      items: [
        { href: '#/overview', label: t('nav.overview'), icon: 'layout-dashboard', match: (p) => p.startsWith('/overview') },
        { href: '#/issues', label: t('nav.issues'), icon: 'triangle-alert', match: (p) => p.startsWith('/issues') },
        { href: '#/performance', label: t('nav.performance'), icon: 'zap', match: (p) => p.startsWith('/performance') },
      ],
    },
    {
      id: 'uptime',
      label: t('nav.group.uptime'),
      items: [
        { href: '#/monitors', label: t('nav.monitors'), icon: 'life-buoy', match: (p) => p.startsWith('/monitors') },
      ],
    },
    {
      id: 'explore',
      label: t('nav.group.explore'),
      items: [
        { href: '#/events', label: t('nav.events'), icon: 'diamond', match: (p) => p.startsWith('/events') },
        { href: '#/transactions', label: t('nav.transactions'), icon: 'timer', match: (p) => p.startsWith('/transactions') },
        { href: '#/sessions', label: t('nav.sessions'), icon: 'clock', match: (p) => p.startsWith('/sessions') },
        { href: '#/users', label: t('nav.users'), icon: 'users', match: (p) => p.startsWith('/users') || p.startsWith('/persons') },
        { href: '#/devices', label: t('nav.devices'), icon: 'monitor-smartphone', match: (p) => p.startsWith('/devices') },
        { href: '#/screens', label: t('nav.screens'), icon: 'layout-panel-top', match: (p) => p.startsWith('/screens') },
        { href: '#/workflows', label: t('nav.workflows'), icon: 'workflow', match: (p) => p.startsWith('/workflows') },
      ],
    },
    {
      id: 'analyze',
      label: t('nav.group.analyze'),
      items: [
        { href: '#/active-users', label: t('nav.activeUsers'), icon: 'users', match: (p) => p.startsWith('/active-users') },
        { href: '#/funnels', label: t('nav.funnels'), icon: 'funnel', match: (p) => p.startsWith('/funnels') },
        { href: '#/journeys', label: t('nav.journeys'), icon: 'waypoints', match: (p) => p.startsWith('/journeys') },
        { href: '#/retention', label: t('nav.retention'), icon: 'repeat', match: (p) => p.startsWith('/retention') },
      ],
    },
    {
      id: 'admin',
      label: t('nav.group.admin'),
      items: [
        { href: '#/admin', label: t('nav.admin'), icon: 'shield-check', match: (p) => p.startsWith('/admin') },
      ],
    },
  ]);

  // Every item renders. An item the member cannot open is LOCKED — inert, with
  // a lock glyph and a tooltip naming the permission — rather than dropped.
  //
  // This used to filter. Hiding is the inverse of the rule `Button` states for
  // action controls ("a user who cannot see a capability cannot learn it exists
  // or ask for it"), and it was justified by the opposite principle in the same
  // codebase. It also hid real gaps: an Admin cannot open four of the twelve
  // admin children, and nothing said so.
  //
  // The lock reason still comes from PAGE_ACCESS, not a per-item `show`
  // predicate. Predicates only ever got written for the items someone
  // remembered: 13 of the 20 below had none, so a member without `event:read`
  // was shown eleven pages that could only ever render an error. One table also
  // means this list and routes.ts can no longer drift, which
  // page-access.test.ts enforces.
  //
  // `item.href` is '#/issues'; slice(1) yields the '/issues' the table is
  // keyed by.
  //
  // '/admin' needs no special case any more. It is deliberately
  // PAGE_ACCESS: null, so it never locks — and `AdminIndex` already renders an
  // explicit empty state for the member who can open none of its children,
  // which is a better answer than the item vanishing.
  const lockedGroups = $derived(
    groups.map((g) => ({
      ...g,
      items: g.items.map((i) => ({ ...i, locked: pageLockedBy(i.href.slice(1)) })),
    })),
  );

  const groupId = (id: string) => `nav-group-${id}`;

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
      const owner = lockedGroups.find((g) => g.items.some((i) => i.match(path)));
      if (owner) navCollapseStore.expand(owner.id);
    });
  });
</script>

<aside class="sidebar">
  <a class="brand" href="#/overview">
    <EyeMark size={28} />
    <span class="wordmark">Sauron</span>
  </a>

  <nav class="nav">
    {#each lockedGroups as group (group.id)}
      {@const collapsed = navCollapseStore.isCollapsed(group.id)}
      <div class="group" class:collapsed>
        <button
          class="group-label"
          type="button"
          aria-expanded={!collapsed}
          aria-controls={groupId(group.id)}
          onclick={() => navCollapseStore.toggle(group.id)}
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
        <div class="items" id={groupId(group.id)}>
          {#each group.items as item (item.href)}
            {#if item.locked}
              <!-- A <button>, not an <a> without href: an anchor with no href is
                   not focusable, and a locked item nobody can focus cannot
                   deliver the tooltip that is the entire reason for showing it.
                   `lockTip` announces it and keeps it reachable. -->
              <button type="button" class="nav-item locked" use:lockTip={item.locked}>
                <span class="ic"><Icon name={item.icon} size={17} /></span>
                <span class="lb">{item.label}</span>
                <span class="lk" aria-hidden="true"><Icon name="lock" size={12} /></span>
              </button>
            {:else}
              <a class="nav-item" class:active={item.match($location)} href={item.href}>
                <span class="ic"><Icon name={item.icon} size={17} /></span>
                <span class="lb">{item.label}</span>
              </a>
            {/if}
          {/each}
        </div>
      </div>
    {/each}
  </nav>

  <div class="bottom">
    <a class="nav-item" class:active={$location.startsWith('/account')} href="#/account">
      <span class="ic"><Icon name="user" size={17} /></span>
      <span class="lb">{t('nav.account')}</span>
    </a>
    <a class="nav-item" class:active={$location.startsWith('/docs')} href="#/docs">
      <span class="ic"><Icon name="book-open" size={17} /></span>
      <span class="lb">{t('nav.docs')}</span>
    </a>
    <div class="foot">
      <span class="foot-label">{t('nav.tagline')}</span>
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
    border-inline-end: 1px solid var(--border);
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
    text-align: start;
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
    margin-inline-start: -3px;
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
  /* A locked item is a <button>, so it needs the reset an <a> does not: the
     global reset only sets font and cursor, leaving the browser's default
     border, background and centred text. */
  .nav-item.locked {
    width: 100%;
    border: 0;
    background: none;
    font: inherit;
    font-weight: 520;
    font-size: 13.5px;
    text-align: start;
    cursor: not-allowed;
    opacity: 0.5;
  }
  .nav-item.locked:hover {
    background: none;
    color: var(--text-muted);
  }
  /* Focus must stay obvious: the whole reason this is a button rather than a
     bare <span> is that a keyboard user can land on it and read the reason. */
  .nav-item.locked:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: -2px;
    opacity: 0.75;
  }
  .lk {
    margin-inline-start: auto;
    display: grid;
    place-items: center;
    flex-shrink: 0;
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
      border-inline-end: none;
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
