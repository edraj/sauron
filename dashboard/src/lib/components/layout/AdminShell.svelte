<script lang="ts">
  import type { Snippet } from 'svelte';
  import { location } from 'svelte-spa-router';
  import AppShell from './AppShell.svelte';
  import Icon from '../ui/Icon.svelte';
  import { visibleAdminNav } from '../../models/admin-nav';

  interface Props {
    requireProject?: boolean;
    requireApp?: boolean;
    children: Snippet;
  }

  // Forwarded to AppShell unchanged — each admin page keeps the scope
  // requirements it had as a top-level route.
  let { requireProject = false, requireApp = false, children }: Props = $props();

  const items = $derived(visibleAdminNav());
</script>

<AppShell {requireProject} {requireApp}>
  <div class="admin">
    <nav class="rail" aria-label="Admin sections">
      {#each items as item (item.href)}
        <a
          href={`#${item.href}`}
          class="item"
          class:active={$location.startsWith(item.href)}
          aria-current={$location.startsWith(item.href) ? 'page' : undefined}
        >
          <Icon name={item.icon} size={15} />
          <span>{item.label}</span>
        </a>
      {/each}
    </nav>
    <div class="body">{@render children()}</div>
  </div>
</AppShell>

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
    top: 0;
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
