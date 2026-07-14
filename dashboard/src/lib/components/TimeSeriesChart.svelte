<script lang="ts">
  import type { SeriesPoint } from '../models';
  import { formatDateTime } from '../utils/format';

  interface Props {
    data: SeriesPoint[];
    height?: number;
    color?: string;
    emptyLabel?: string;
    format?: (n: number) => string;
    showTotal?: boolean;
  }

  let {
    data,
    height = 160,
    color = 'var(--primary)',
    emptyLabel = 'No data in this range',
    format = (n: number) => n.toLocaleString(),
    showTotal = true,
  }: Props = $props();

  const max = $derived(data.length ? Math.max(...data.map((d) => d.count), 1) : 1);
  const total = $derived(data.reduce((sum, d) => sum + d.count, 0));

  function barHeight(count: number): number {
    if (max <= 0) return 0;
    // Give even 0-count buckets a hair of presence, real bars a floor of 4%.
    return count === 0 ? 2 : Math.max(4, (count / max) * 100);
  }

  function label(bucket: string): string {
    const d = new Date(bucket);
    if (Number.isNaN(d.getTime())) return bucket;
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }
</script>

{#if data.length === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px" style:--bar-color={color}>
      {#each data as point (point.bucket)}
        <div class="col" title={`${formatDateTime(point.bucket)} · ${format(point.count)}`}>
          <div class="bar" style="height:{barHeight(point.count)}%">
            <span class="tip">{format(point.count)} · {label(point.bucket)}</span>
          </div>
        </div>
      {/each}
    </div>
    <div class="axis">
      <span>{label(data[0].bucket)}</span>
      {#if showTotal}<span class="total">{total.toLocaleString()} total</span>{/if}
      <span>{label(data[data.length - 1].bucket)}</span>
    </div>
  </div>
{/if}

<style>
  .chart {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .plot {
    display: flex;
    align-items: flex-end;
    gap: 3px;
    padding: 4px 2px 0;
    border-bottom: 1px solid var(--border);
  }
  .col {
    flex: 1;
    min-width: 3px;
    height: 100%;
    display: flex;
    align-items: flex-end;
    justify-content: center;
  }
  .bar {
    position: relative;
    width: 100%;
    max-width: 42px;
    border-radius: 3px 3px 0 0;
    background: linear-gradient(
      to top,
      color-mix(in srgb, var(--bar-color) 55%, transparent),
      var(--bar-color)
    );
    transition: filter 0.12s ease, transform 0.12s ease;
  }
  .col:hover .bar {
    filter: brightness(1.18);
  }
  .tip {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 50%;
    transform: translateX(-50%);
    padding: 4px 8px;
    background: var(--surface-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    font-size: 11px;
    white-space: nowrap;
    color: var(--text);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
    z-index: 2;
    box-shadow: var(--shadow);
  }
  .col:hover .tip {
    opacity: 1;
  }
  .axis {
    display: flex;
    justify-content: space-between;
    font-size: 11px;
    color: var(--text-faint);
  }
  .axis .total {
    color: var(--text-muted);
    font-weight: 560;
  }
  .chart-empty {
    display: grid;
    place-items: center;
    color: var(--text-faint);
    font-size: 13px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
</style>
