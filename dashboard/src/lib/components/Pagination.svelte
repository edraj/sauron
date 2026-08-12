<script lang="ts">
  import Icon from './ui/Icon.svelte';

  interface Props {
    offset: number;
    limit: number;
    /** Number of rows on the current page. */
    count: number;
    /**
     * Whether a page exists after this one.
     *
     * Supplied by the caller rather than inferred from `count >= limit`, which
     * was wrong: a final page holding exactly `limit` rows offered an enabled
     * Next that led to an empty page. The caller knows the answer — from a
     * total, or by requesting `limit + 1` rows and rendering `limit`.
     */
    hasNext: boolean;
    onchange: (offset: number) => void;
  }

  let { offset, limit, count, hasNext, onchange }: Props = $props();

  const from = $derived(count === 0 ? 0 : offset + 1);
  const to = $derived(offset + count);
  const hasPrev = $derived(offset > 0);
  const currentPage = $derived(Math.floor(offset / limit) + 1);
</script>

<div class="pager">
  <span class="range muted">
    {#if count === 0 && offset === 0}No results{:else if count === 0}End of results{:else}{from.toLocaleString()}–{to.toLocaleString()}{/if}
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

    {#if currentPage > 2}
      <button class="pg num" onclick={() => onchange(0)} type="button">1</button>
      {#if currentPage > 3}
        <span class="ellipsis">...</span>
      {/if}
    {/if}

    {#if currentPage > 1}
      <button class="pg num" onclick={() => onchange((currentPage - 2) * limit)} type="button">{currentPage - 1}</button>
    {/if}

    <button class="pg num active" type="button">{currentPage}</button>

    {#if hasNext}
      <button class="pg num" onclick={() => onchange(currentPage * limit)} type="button">{currentPage + 1}</button>
    {/if}

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
    align-items: center;
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
  .pg.num {
    padding: 6px 10px;
    min-width: 32px;
    justify-content: center;
  }
  .pg:hover:not(:disabled, .active) {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .pg.active {
    background: var(--surface-3);
    color: var(--text);
    border-color: var(--border-strong);
    cursor: default;
  }
  .pg:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .ellipsis {
    color: var(--text-muted);
    padding: 0 2px;
    font-size: 12px;
    user-select: none;
  }
</style>
