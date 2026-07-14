<script lang="ts">
  interface Props {
    data: Record<string, unknown> | null | undefined;
    emptyLabel?: string;
  }

  let { data, emptyLabel = 'None' }: Props = $props();

  const entries = $derived(
    data && typeof data === 'object' ? Object.entries(data) : [],
  );

  function render(value: unknown): string {
    if (value === null || value === undefined) return '—';
    if (typeof value === 'object') return JSON.stringify(value);
    return String(value);
  }
</script>

{#if entries.length === 0}
  <p class="kv-empty muted">{emptyLabel}</p>
{:else}
  <dl class="kv">
    {#each entries as [key, value] (key)}
      <div class="kv-row">
        <dt class="mono">{key}</dt>
        <dd class="mono">{render(value)}</dd>
      </div>
    {/each}
  </dl>
{/if}

<style>
  .kv-empty {
    font-size: 13px;
    padding: 2px 0;
  }
  .kv {
    display: flex;
    flex-direction: column;
    margin: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .kv-row {
    display: grid;
    grid-template-columns: minmax(120px, 34%) 1fr;
    gap: 12px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
  }
  .kv-row:last-child {
    border-bottom: none;
  }
  dt {
    font-size: 12px;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  dd {
    margin: 0;
    font-size: 12px;
    color: var(--text);
    word-break: break-word;
  }
</style>
