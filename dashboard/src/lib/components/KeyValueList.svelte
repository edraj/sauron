<script lang="ts">
  import { t } from '../i18n';

  interface Props {
    data: Record<string, unknown> | null | undefined;
    emptyLabel?: string;
  }

  let { data, emptyLabel }: Props = $props();

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
  <p class="kv-empty muted">{emptyLabel ?? t('common.none')}</p>
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
    font-size: var(--text-sm);
    padding: 2px 0;
  }
  .kv {
    display: flex;
    flex-direction: column;
    margin: 0;
  }
  .kv-row {
    display: grid;
    grid-template-columns: minmax(96px, 34%) minmax(0, 1fr);
    gap: 12px;
    padding: 7px 2px;
    border-bottom: 1px solid var(--border);
  }
  .kv-row:last-child {
    border-bottom: none;
  }
  dt {
    font-size: var(--text-2xs);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  dd {
    margin: 0;
    font-size: var(--text-2xs);
    color: var(--text);
    word-break: break-word;
  }
</style>
