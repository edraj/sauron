<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon, { type IconName } from './Icon.svelte';

  interface Props {
    title: string;
    description?: string;
    icon?: IconName;
    action?: Snippet;
  }

  let { title, description, icon = 'inbox', action }: Props = $props();
</script>

<div class="empty">
  <div class="glyph" aria-hidden="true"><Icon name={icon} size={26} /></div>
  <h3 class="title">{title}</h3>
  {#if description}<p class="desc">{description}</p>{/if}
  {#if action}<div class="action">{@render action()}</div>{/if}
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 56px 24px;
    gap: 8px;
  }
  .glyph {
    width: 56px;
    height: 56px;
    display: grid;
    place-items: center;
    font-size: 26px;
    border-radius: 50%;
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-faint);
    margin-bottom: 6px;
  }
  .title {
    font-size: 16px;
    font-weight: 620;
  }
  .desc {
    color: var(--text-muted);
    max-width: 380px;
    font-size: 13.5px;
  }
  .action {
    margin-top: 10px;
  }
</style>
