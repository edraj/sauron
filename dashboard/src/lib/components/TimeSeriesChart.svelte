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
    /**
     * Override how a bucket is rendered on the axis AND in the tooltip.
     *
     * The default path parses `bucket` as a Date and renders it in the
     * VIEWER's zone. That is correct for the timestamp buckets every existing
     * caller passes, and wrong for a pure `YYYY-MM-DD` calendar day: parsing
     * is UTC, rendering is not, so in `America/New_York` the bar for
     * `2026-07-31` is labelled "Jul 30" and its tooltip reads "Jul 30, 2026,
     * 08:00 PM" — a time of day on a bucket that has none. The active-users
     * page passes `utcDayLabel` so its chart, its CSV and its filename cannot
     * disagree about which day a number belongs to.
     */
    label?: (bucket: string) => string;
  }

  let {
    data,
    height = 160,
    color = 'var(--primary)',
    emptyLabel = 'No data in this range',
    format = (n: number) => n.toLocaleString(),
    showTotal = true,
    label: labelProp,
  }: Props = $props();

  const max = $derived(data.length ? Math.max(...data.map((d) => d.count), 1) : 1);
  const total = $derived(data.reduce((sum, d) => sum + d.count, 0));

  const hasSegments = $derived(data.length > 0 && data[0].segments && data[0].segments.length > 0);
  const segmentsDefinition = $derived(hasSegments ? data[0].segments! : []);

  function barHeight(count: number): number {
    if (max <= 0) return 0;
    // Give even 0-count buckets a hair of presence, real bars a floor of 4%.
    return count === 0 ? 2 : Math.max(4, (count / max) * 100);
  }

  function label(bucket: string): string {
    if (labelProp) return labelProp(bucket);
    const d = new Date(bucket);
    if (Number.isNaN(d.getTime())) return bucket;
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  // The hover title uses the same function when the prop is supplied;
  // `formatDateTime` would put a time of day on a calendar-day bucket.
  function tooltip(point: SeriesPoint): string {
    const base = labelProp ? labelProp(point.bucket) : formatDateTime(point.bucket);
    const main = `${base} · ${format(point.count)}`;
    if (point.segments && point.segments.length > 0) {
      const segs = point.segments.map(s => `${s.label || ''}: ${format(s.count)}`).join(', ');
      return `${main} (${segs})`;
    }
    return main;
  }
</script>

{#if data.length === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px" style:--bar-color={color}>
      <!-- `role="img"` + `aria-label` rather than `title`: `title` renders the
           browser's own dark tooltip, which duplicated the styled one on every
           hover (the bar read "10 · Jul 29" above and "Jul 29 · 10" below at
           the same time). `aria-label` keeps the same text as the accessible
           name without drawing a second box. -->
      {#each data as point (point.bucket)}
        <div class="col" role="img" aria-label={tooltip(point)}>
          <div class="bar" class:has-segments={point.segments && point.segments.length > 0} style="height:{barHeight(point.count)}%">
            <div class="bar-fill">
              {#if point.segments && point.segments.length > 0}
                {#each point.segments as seg}
                  <div class="segment" style="height: {(seg.count / point.count) * 100}%; background-color: {seg.color || 'var(--bar-color)'}"></div>
                {/each}
              {/if}
            </div>
            <div class="tip tip-value" class:detailed={point.segments && point.segments.length > 0}>
              <div class="total-line">{format(point.count)}{#if point.segments && point.segments.length > 0} total{/if}</div>
              {#if point.segments && point.segments.length > 0}
                <div class="segments-list">
                  {#each point.segments as seg}
                    <div class="seg-line">
                      <span class="swatch" style="background-color: {seg.color || 'var(--bar-color)'}"></span>
                      <span class="seg-label">{seg.label || ''}:</span>
                      <span class="seg-value">{format(seg.count)}</span>
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          </div>
          <span class="tip tip-date">{label(point.bucket)}</span>
        </div>
      {/each}
    </div>
    <div class="axis">
      <span>{label(data[0].bucket)}</span>
      {#if showTotal}<span class="total">{total.toLocaleString()} total</span>{/if}
      <span>{label(data[data.length - 1].bucket)}</span>
    </div>
    {#if hasSegments}
      <div class="legend">
        {#each segmentsDefinition as seg}
          <div class="legend-item">
            <span class="swatch" style="background-color: {seg.color || 'var(--bar-color)'}"></span>
            <span class="legend-label">{seg.label}</span>
          </div>
        {/each}
      </div>
    {/if}
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
    position: relative;
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
    transition: filter 0.12s ease, transform 0.12s ease;
  }
  .bar-fill {
    position: absolute;
    inset: 0;
    border-radius: 3px 3px 0 0;
    background: linear-gradient(
      to top,
      color-mix(in srgb, var(--bar-color) 55%, transparent),
      var(--bar-color)
    );
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    overflow: hidden;
  }
  .bar.has-segments .bar-fill {
    background: transparent;
  }
  .segment {
    width: 100%;
  }
  .col:hover .bar {
    filter: brightness(1.18);
  }
  /* Two labels per bar: the count above it, the date beneath the axis line.
     Both are absolutely positioned so appearing on hover never reflows the
     chart, and both are centred on the bar so they read as one pair. */
  .tip {
    position: absolute;
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
  /* Anchored to the BAR, so it tracks the bar's top rather than floating at a
     fixed height — the number sits just above however tall the bar is. */
  .tip-value {
    bottom: calc(100% + 6px);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .tip-value.detailed {
    align-items: stretch;
    min-width: 120px;
    padding: 6px 10px;
  }
  .total-line {
    text-align: center;
  }
  .tip-value.detailed .total-line {
    border-bottom: 1px solid var(--border);
    padding-bottom: 4px;
    margin-bottom: 4px;
    text-align: left;
  }
  .segments-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-weight: normal;
  }
  .seg-line {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .swatch {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
  }
  .seg-label {
    flex: 1;
    color: var(--text-muted);
  }
  .seg-value {
    font-weight: 600;
  }
  
  .legend {
    display: flex;
    justify-content: center;
    gap: 16px;
    margin-top: 4px;
    font-size: 11px;
    color: var(--text-muted);
  }
  .legend-item {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  /* Anchored to the COLUMN, whose bottom is the axis line, so every date sits
     on one baseline instead of stepping up and down with the bars. It overlays
     the axis row on hover; that row is static text, and overlaying keeps the
     layout from shifting. */
  .tip-date {
    top: calc(100% + 6px);
    color: var(--text-muted);
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
