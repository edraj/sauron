<script lang="ts">
  import { t } from '../../i18n';
  import type { Snippet } from 'svelte';
  import { location } from 'svelte-spa-router';
  import Icon from '../ui/Icon.svelte';
  import { adminNavLocks } from '../../models/admin-nav';
  import { lockTip } from '../../actions/lock-tip';

  interface Props {
    children: Snippet;
  }

  // No AppShell here any more, and no requireProject/requireApp props: the
  // one shell lives in App.svelte and reads each admin route's flags from
  // `models/shell.ts`. This component is only the admin rail + body grid.
  let { children }: Props = $props();

  // Every child renders, locked ones included — see `adminNavLocks`. The rail
  // and the sidebar read the same helper, so they cannot disagree about which
  // pages exist or about which permission each one needs.
  const items = $derived(adminNavLocks());
</script>

<div class="admin">
    <nav class="rail" aria-label={t('shell.adminSections')}>
      {#each items as item (item.href)}
        {#if item.locked}
          <button type="button" class="item locked" use:lockTip={item.locked}>
            <Icon name={item.icon} size={15} />
            <span>{item.label}</span>
            <span class="lk" aria-hidden="true"><Icon name="lock" size={12} /></span>
          </button>
        {:else}
          <a
            href={`#${item.href}`}
            class="item"
            class:active={$location.startsWith(item.href)}
            aria-current={$location.startsWith(item.href) ? 'page' : undefined}
          >
            <Icon name={item.icon} size={15} />
            <span>{item.label}</span>
          </a>
        {/if}
      {/each}
    </nav>
    <div class="body">{@render children()}</div>
</div>

<style>
  .admin {
    display: grid;
    grid-template-columns: 190px minmax(0, 1fr);
    gap: 22px;
    align-items: start;
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: 2px;
    position: sticky;
    /* Below the topbar, not at viewport 0. The topbar is itself sticky and
       paints over this column, so `top: 0` parks the first rail item
       underneath it. Same offset the Docs table of contents uses. */
    top: calc(var(--topbar-h) + 16px);
    align-self: start;
    /* A rail taller than the viewport has to scroll on its own, or its lower
       entries become unreachable once it is pinned. */
    max-height: calc(100vh - var(--topbar-h) - 32px);
    overflow-y: auto;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 10px;
    border-radius: var(--radius);
    font-size: 13px;
    color: var(--text-muted);
    text-decoration: none;
  }
  .item:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  /* A locked entry is a <button>: the global reset only sets font and cursor,
     so the browser default border, background and centred text all need
     clearing to match the <a> beside it. */
  .item.locked {
    width: 100%;
    border: 0;
    background: none;
    font: inherit;
    font-size: 13px;
    text-align: start;
    cursor: not-allowed;
    opacity: 0.5;
  }
  .item.locked:hover {
    background: none;
    color: var(--text-muted);
  }
  .item.locked:focus-visible {
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
  .item.active {
    background: var(--surface-2);
    color: var(--text);
    font-weight: 560;
  }
  .body {
    min-width: 0;
  }
  @media (max-width: 900px) {
    .admin {
      grid-template-columns: 1fr;
    }
    .rail {
      flex-direction: row;
      overflow-x: auto;
      position: static;
    }
  }
</style>
