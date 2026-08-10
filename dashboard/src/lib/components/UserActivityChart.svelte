<script lang="ts">
  import type { UserSeriesPoint } from '../models';
  import { formatDateTime } from '../utils/format';

  interface Props {
    data: UserSeriesPoint[];
    height?: number;
    emptyLabel?: string;
  }

  let { data, height = 180, emptyLabel = 'No user activity in this range' }: Props = $props();

  /**
   * ONE scale across both series.
   *
   * New users used to be a polyline normalised to its own maximum, which is
   * defensible for an overlay but not for a bar: two bar series on independent
   * scales put a day with 3 new users level with a day of 300 active ones. New
   * is a subset of active, so a shared maximum is both honest and the one that
   * makes the pair comparable at a glance.
   */
  const scaleMax = $derived(
    data.length ? Math.max(...data.map((d) => Math.max(d.active, d.new_users)), 1) : 1,
  );

  function barHeight(v: number): number {
    if (scaleMax <= 0) return 0;
    // A zero day keeps a 2% stub so the column still reads as a day that
    // existed and had none, rather than as missing data.
    return v === 0 ? 2 : Math.max(4, (v / scaleMax) * 100);
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
    <div class="plot" style="height:{height}px">
      <div class="bars">
        {#each data as point (point.bucket)}
          <div
            class="col"
            title={`${formatDateTime(point.bucket)} · ${point.active} active · ${point.new_users} new`}
          >
            <div class="pair">
              <div class="bar active" style="height:{barHeight(point.active)}%">
                <!-- Anchored to the active bar, which is the taller of the two
                     (new is a subset), so the tip clears both. -->
                <span class="tip">{point.active} active · {point.new_users} new<br />{label(point.bucket)}</span>
              </div>
              <div class="bar new" style="height:{barHeight(point.new_users)}%"></div>
            </div>
          </div>
        {/each}
      </div>
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
  .col {
    position: relative;
    flex: 1;
    min-width: 3px;
    height: 100%;
    display: flex;
    align-items: flex-end;
    justify-content: center;
  }
  /* The two bars for one day. `gap` is 1px rather than the 3px between days so
     a pair still reads as a pair at 90-day ranges, where each column is only a
     few pixels wide. */
  .pair {
    display: flex;
    align-items: flex-end;
    justify-content: center;
    gap: 1px;
    width: 100%;
    max-width: 42px;
    height: 100%;
  }
  .bar {
    flex: 1;
    min-width: 0;
    border-radius: 3px 3px 0 0;
    transition: filter 0.12s ease;
  }
  .bar.active {
    position: relative; /* positioning context for .tip */
    background: linear-gradient(to top, color-mix(in srgb, var(--primary) 55%, transparent), var(--primary));
  }
  .bar.new {
    background: linear-gradient(to top, color-mix(in srgb, var(--info) 55%, transparent), var(--info));
  }
  .col:hover .bar { filter: brightness(1.18); }
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
