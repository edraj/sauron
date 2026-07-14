<script lang="ts">
  import type { HistoBucket } from '../models';

  interface Props {
    data: HistoBucket[];
    height?: number;
    emptyLabel?: string;
  }

  let { data, height = 160, emptyLabel = 'No sessions in this range' }: Props = $props();

  const max = $derived(data.length ? Math.max(...data.map((d) => d.count), 1) : 1);
  const total = $derived(data.reduce((sum, d) => sum + d.count, 0));

  function barHeight(count: number): number {
    if (max <= 0) return 0;
    return count === 0 ? 2 : Math.max(4, (count / max) * 100);
  }
</script>

{#if total === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px">
      {#each data as b (b.bucket)}
        <div class="col" title={`${b.bucket}: ${b.count.toLocaleString()} sessions`}>
          <div class="bar" style="height:{barHeight(b.count)}%">
            <span class="cnt">{b.count.toLocaleString()}</span>
          </div>
          <span class="lbl">{b.bucket}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .chart { display: flex; flex-direction: column; gap: 8px; }
  .plot { display: flex; align-items: flex-end; gap: 10px; padding: 16px 4px 0; }
  .col { flex: 1; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: flex-end; gap: 6px; }
  .bar {
    position: relative;
    width: 100%;
    max-width: 64px;
    border-radius: 4px 4px 0 0;
    background: linear-gradient(to top, color-mix(in srgb, var(--primary) 55%, transparent), var(--primary));
  }
  .cnt {
    position: absolute;
    bottom: calc(100% + 3px);
    left: 50%;
    transform: translateX(-50%);
    font-size: 11px;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .lbl { font-size: 11.5px; color: var(--text-faint); white-space: nowrap; }
  .chart-empty {
    display: grid;
    place-items: center;
    color: var(--text-faint);
    font-size: 13px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
</style>
