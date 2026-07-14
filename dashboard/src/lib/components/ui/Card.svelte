<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    padding?: 'none' | 'sm' | 'md' | 'lg';
    title?: string;
    class?: string;
    header?: Snippet;
    actions?: Snippet;
    children: Snippet;
  }

  let {
    padding = 'md',
    title,
    class: klass = '',
    header,
    actions,
    children,
  }: Props = $props();
</script>

<section class="card {klass}">
  {#if title || header || actions}
    <header class="card-head">
      <div class="head-left">
        {#if header}{@render header()}{:else if title}<h3 class="card-title">{title}</h3>{/if}
      </div>
      {#if actions}<div class="head-actions">{@render actions()}</div>{/if}
    </header>
  {/if}
  <div class="card-body pad-{padding}">
    {@render children()}
  </div>
</section>

<style>
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-sm);
    overflow: hidden;
  }
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 18px;
    border-bottom: 1px solid var(--border);
  }
  .card-title {
    font-size: 14.5px;
    font-weight: 620;
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pad-none {
    padding: 0;
  }
  .pad-sm {
    padding: 12px;
  }
  .pad-md {
    padding: 18px;
  }
  .pad-lg {
    padding: 24px;
  }
</style>
