<script lang="ts">
  import Icon from './ui/Icon.svelte';

  interface Props {
    offset: number;
    limit: number;
    // Number of rows on the current page — used to detect the last page.
    count: number;
    onchange: (offset: number) => void;
  }

  let { offset, limit, count, onchange }: Props = $props();

  const from = $derived(count === 0 ? 0 : offset + 1);
  const to = $derived(offset + count);
  const hasPrev = $derived(offset > 0);
  const hasNext = $derived(count >= limit);
</script>

<div class="pager">
  <span class="range muted">
    {#if count === 0}No results{:else}{from.toLocaleString()}–{to.toLocaleString()}{/if}
  </span>
  <div class="btns">
    <button
      class="pg"
      disabled={!hasPrev}
      onclick={() => onchange(Math.max(0, offset - limit))}
      type="button"
    >
      <Icon name="chevron-left" size={14} /> Prev
    </button>
    <button
      class="pg"
      disabled={!hasNext}
      onclick={() => onchange(offset + limit)}
      type="button"
    >
      Next <Icon name="chevron-right" size={14} />
    </button>
  </div>
</div>

<style>
  .pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 2px 0;
  }
  .range {
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
  }
  .btns {
    display: flex;
    gap: 6px;
  }
  .pg {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 6px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 550;
    transition: color 0.12s ease, border-color 0.12s ease;
  }
  .pg:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .pg:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
