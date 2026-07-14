<script lang="ts">
  import type { UserSeriesPoint } from '../models';
  import { formatDateTime } from '../utils/format';

  interface Props {
    data: UserSeriesPoint[];
    height?: number;
    emptyLabel?: string;
  }

  let { data, height = 180, emptyLabel = 'No user activity in this range' }: Props = $props();

  const maxActive = $derived(data.length ? Math.max(...data.map((d) => d.active), 1) : 1);
  const maxNew = $derived(data.length ? Math.max(...data.map((d) => d.new_users), 1) : 1);

  function barHeight(v: number): number {
    if (maxActive <= 0) return 0;
    return v === 0 ? 2 : Math.max(4, (v / maxActive) * 100);
  }

  function label(bucket: string): string {
    const d = new Date(bucket);
    if (Number.isNaN(d.getTime())) return bucket;
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  // New-users overlay as a polyline in a 0..100 viewBox (y inverted).
  const linePoints = $derived(
    data
      .map((d, i) => {
        const x = data.length === 1 ? 50 : (i / (data.length - 1)) * 100;
        const y = 100 - (maxNew <= 0 ? 0 : (d.new_users / maxNew) * 100);
        return `${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(' '),
  );
</script>

{#if data.length === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px">
      <div class="bars">
        {#each data as point (point.bucket)}
          <div
            class="col"
            title={`${formatDateTime(point.bucket)} · ${point.active} active · ${point.new_users} new`}
          >
            <div class="bar" style="height:{barHeight(point.active)}%">
              <span class="tip">{point.active} active · {point.new_users} new<br />{label(point.bucket)}</span>
            </div>
          </div>
        {/each}
      </div>
      {#if maxNew > 0}
        <svg class="overlay" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
          <polyline points={linePoints} fill="none" stroke="var(--info)" stroke-width="1.5" vector-effect="non-scaling-stroke" />
        </svg>
      {/if}
    </div>
    <div class="axis">
      <span>{label(data[0].bucket)}</span>
      <span class="legend"><i class="k a"></i> active <i class="k n"></i> new</span>
      <span>{label(data[data.length - 1].bucket)}</span>
    </div>
  </div>
{/if}

<style>
  .chart { display: flex; flex-direction: column; gap: 8px; }
  .plot { position: relative; }
  .bars {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: flex-end;
    gap: 3px;
    padding: 4px 2px 0;
    border-bottom: 1px solid var(--border);
  }
  .col { flex: 1; min-width: 3px; height: 100%; display: flex; align-items: flex-end; justify-content: center; }
  .bar {
    position: relative;
    width: 100%;
    max-width: 42px;
    border-radius: 3px 3px 0 0;
    background: linear-gradient(to top, color-mix(in srgb, var(--primary) 55%, transparent), var(--primary));
    transition: filter 0.12s ease;
  }
  .col:hover .bar { filter: brightness(1.18); }
  .overlay { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; }
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
  .col:hover .tip { opacity: 1; }
  .axis { display: flex; justify-content: space-between; align-items: center; font-size: 11px; color: var(--text-faint); }
  .legend { display: inline-flex; align-items: center; gap: 6px; color: var(--text-muted); }
  .k { display: inline-block; width: 9px; height: 9px; border-radius: 2px; vertical-align: middle; }
  .k.a { background: var(--primary); }
  .k.n { background: var(--info); }
  .chart-empty {
    display: grid;
    place-items: center;
    color: var(--text-faint);
    font-size: 13px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
</style>
