<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon, { type IconName } from './Icon.svelte';

  interface Props {
    title: string;
    description?: string;
    icon?: IconName;
    action?: Snippet;
    /** `page` stands alone with its icon; `section` is the compact in-section form. */
    size?: 'page' | 'section';
  }

  let { title, description, icon = 'inbox', action, size = 'page' }: Props = $props();
</script>

<div class="empty {size}">
  {#if size === 'page'}
    <div class="glyph" aria-hidden="true"><Icon name={icon} size={15} /></div>
  {/if}
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
    gap: 4px;
  }
  .empty.page {
    padding: 40px 24px;
  }
  .empty.section {
    padding: 20px 16px;
  }
  .glyph {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    border: 1px solid var(--border-strong);
    color: var(--text-muted);
    margin-bottom: 8px;
  }
  .title {
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
  }
  .section .title {
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .desc {
    color: var(--text-muted);
    max-width: 44ch;
    font-size: var(--text-sm);
  }
  .action {
    margin-top: 10px;
  }
</style>
