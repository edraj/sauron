<script lang="ts">
  import { t, formatNumber, formatCompact } from '../i18n';
  import { lifecycleBars, lifecycleLayout } from '../models/retention';
  import type { LifecyclePoint } from '../models/retention';

  interface Props {
    points: LifecyclePoint[];
  }

  let { points }: Props = $props();

  const bars = $derived(lifecycleBars(points));
  const layout = $derived(lifecycleLayout(bars));

  const SERIES = [
    { key: 'new' as const, label: 'retention.lifecycle.new', color: 'var(--primary)' },
    { key: 'returning' as const, label: 'retention.lifecycle.returning', color: 'var(--success)' },
    {
      key: 'resurrected' as const,
      label: 'retention.lifecycle.resurrected',
      color: 'var(--warning)',
    },
  ];

  /** A positive value's height as % of the POSITIVE region. */
  function upShare(v: number): number {
    return (v / layout.posTop) * 100;
  }

  /** A dormant value's height as % of the NEGATIVE region. */
  function downShare(v: number): number {
    return layout.negTop === 0 ? 0 : (Math.abs(v) / layout.negTop) * 100;
  }

  /** A positive axis value's distance from the plot BOTTOM, as % of the plot. */
  function plotBottom(v: number): number {
    return (layout.negShare + (v / layout.posTop) * layout.posShare) * 100;
  }

  /**
   * Label roughly every nth bar so the axis stays readable at any width;
   * first and last are always labelled. Hover titles carry the exact date on
   * every bar either way.
   */
  const labelEvery = $derived(Math.max(1, Math.ceil(bars.length / 6)));
  function showLabel(i: number): boolean {
    return i === 0 || i === bars.length - 1 || i % labelEvery === 0;
  }

  /** "08-22" — month-day is enough beside hover tips carrying the full date. */
  function shortDate(iso: string): string {
    return iso.slice(5);
  }

  /**
   * Where to pin the hover tip for a bucket.
   *
   * Above the bar normally, but FLIPPED to hang just inside the plot once the
   * bar's top passes ~62% of the height — otherwise a full-scale bar (the
   * common case here, since the tallest bar sets the scale) pushes the tip out
   * through the top of the chart and over the card header, where it is clipped.
   * Purely arithmetic on values already computed, so there is no measurement
   * and no hidden-pane failure mode.
   */
  function tipVertical(b: (typeof bars)[number]): string {
    const top = plotBottom(b.active);
    return top > 62 ? `top:calc(${100 - top}% + 8px)` : `bottom:calc(${top}% + 8px)`;
  }

  /**
   * Keep the tip inside the plot horizontally: centred over its column, except
   * for the first and last two, where a centred box would spill past the edge
   * and be clipped by the card.
   */
  function tipHorizontal(i: number): string {
    if (i <= 1) return 'inset-inline-start:0;transform:none';
    if (i >= bars.length - 2) return 'inset-inline-start:auto;inset-inline-end:0;transform:none';
    return '';
  }

  /** The four series of one bucket, in stack order, for the hover tip. */
  function rows(b: (typeof bars)[number]) {
    return [
      ...SERIES.map((s) => ({
        label: t(s.label as never),
        color: s.color,
        value: b.positive.find((p) => p.key === s.key)?.value ?? 0,
      })),
      {
        label: t('retention.lifecycle.dormant'),
        color: 'var(--danger)',
        value: -b.dormant,
      },
    ];
  }
</script>

<!--
  New / returning / resurrected stacked above the zero line, dormant below it.

  Sized entirely from props and percentages — never from getBoundingClientRect
  inside requestAnimationFrame. A chart that measures itself that way renders
  nothing forever when its pane is `display: none`, because rAF never fires
  there; this project has hit that already.

  The vertical geometry comes from `lifecycleLayout`: the positive and
  negative regions share the plot in proportion to their scales instead of
  50/50, which is what put the date axis a chart-height away from the bars
  when dormancy was small (the 2026-08-29 feedback). Every bar carries its
  active total above it and the dormant count below; per-segment values render
  inline only when the segment is tall enough to hold them, with the exact
  numbers on hover either way.
-->
<div class="lifecycle">
  <div class="chart">
    <div class="yaxis" aria-hidden="true">
      {#each layout.posTicks as tick (tick)}
        <span class="tick" style="bottom:{plotBottom(tick)}%">{formatCompact(tick)}</span>
      {/each}
      {#if layout.negTop > 0}
        <span class="tick neg" style="bottom:0%">-{formatCompact(layout.negTop)}</span>
      {/if}
    </div>
    <div class="plot">
      {#each layout.posTicks as tick (tick)}
        <div
          class="gridline"
          class:zero={tick === 0}
          style="bottom:{plotBottom(tick)}%"
        ></div>
      {/each}
      {#if layout.negTop > 0}
        <div class="gridline" style="bottom:0%"></div>
      {/if}
      <div class="cols">
        {#each bars as b, i (b.start)}
          <!-- `title` stays alongside the styled tip, exactly as
               UserActivityChart does: CSS :hover never fires for keyboard or
               screen-reader users, so the native tooltip is the only version
               of this data they get. -->
          <div
            class="col"
            title={`${b.start} · ${formatNumber(b.active)} ${t('retention.lifecycle.active')}`}
          >
            <div class="up" style="height:{layout.posShare * 100}%">
              {#each SERIES as s (s.key)}
                {@const v = b.positive.find((p) => p.key === s.key)?.value ?? 0}
                {#if v > 0}
                  <div
                    class="seg"
                    data-series={s.key}
                    data-sign="positive"
                    style="height:{upShare(v)}%;background:{s.color}"
                  ></div>
                {/if}
              {/each}
            </div>
            <div class="down" style="height:{layout.negShare * 100}%">
              {#if b.dormant < 0}
                <div
                  class="seg"
                  data-series="dormant"
                  data-sign="negative"
                  style="height:{downShare(b.dormant)}%"
                ></div>
              {/if}
            </div>
            <!-- Anchored to the top of THIS bar's stack, so it sits just clear
                 of the bar like the DAU chart's. Clamped so a full-scale bar
                 cannot push it out of the plot. -->
            <div class="tip" style="{tipVertical(b)};{tipHorizontal(i)}">
              <div class="tip-date">{b.start}</div>
              {#each rows(b) as r (r.label)}
                <div class="tip-row">
                  <span class="tip-swatch" style="background:{r.color}"></span>
                  <span class="tip-label">{r.label}</span>
                  <span class="tip-value">{formatNumber(r.value)}</span>
                </div>
              {/each}
              <div class="tip-row tip-total">
                <span class="tip-swatch" style="background:transparent"></span>
                <span class="tip-label">{t('retention.lifecycle.active')}</span>
                <span class="tip-value">{formatNumber(b.active)}</span>
              </div>
            </div>
          </div>
        {/each}
      </div>
    </div>
  </div>

  <!-- The axis dates the bars. Offset by the y-axis gutter so labels sit
       directly under their columns. -->
  <div class="labels" aria-hidden="true">
    {#each bars as b, i (b.start)}
      <div class="label-slot">
        {#if showLabel(i)}<span>{shortDate(b.start)}</span>{/if}
      </div>
    {/each}
  </div>

  <ul class="legend">
    {#each SERIES as s (s.key)}
      <li><span class="swatch" style="background:{s.color}"></span>{t(s.label as never)}</li>
    {/each}
    <li>
      <span class="swatch dormant-swatch"></span>{t('retention.lifecycle.dormant')}
    </li>
  </ul>
</div>

<style>
  .lifecycle {
    width: 100%;
    /* One number, three consumers: the gutter, the labels offset, nothing
       measured. */
    --yaxis-width: 48px;
  }

  .chart {
    display: flex;
    height: 210px;
  }

  .yaxis {
    position: relative;
    flex: 0 0 var(--yaxis-width);
    font-size: 0.6875rem;
    color: var(--muted-fg);
  }

  .tick {
    position: absolute;
    inset-inline-start: 0;
    inset-inline-end: 6px;
    transform: translateY(50%);
    text-align: end;
    font-variant-numeric: tabular-nums;
  }

  .tick.neg {
    color: var(--danger);
    opacity: 0.8;
  }

  .plot {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
  }

  .gridline {
    position: absolute;
    inset-inline: 0;
    height: 1px;
    background: var(--border);
    opacity: 0.5;
  }

  /* The zero line is the axis: solid, above the tinted gridlines. */
  .gridline.zero {
    opacity: 1;
    z-index: 1;
  }

  /*
   * `overflow-x: auto` here would CLIP the hover tip, which overflows the
   * column vertically by design. The columns flex to fit instead — the chart
   * has at most 52 buckets and each keeps a min-width, so there is nothing to
   * scroll that the tip should be sacrificed for.
   */
  .cols {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: stretch;
    gap: 4px;
  }

  .col {
    position: relative;
    flex: 1 1 0;
    min-width: 24px;
    display: flex;
    flex-direction: column;
  }

  .up {
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
  }

  .down {
    display: flex;
    flex-direction: column;
    justify-content: flex-start;
  }

  .seg {
    width: 100%;
    transition: filter 0.12s ease;
  }

  .seg[data-series='dormant'] {
    background: var(--danger);
    opacity: 0.55;
  }

  /* Same affordance as UserActivityChart: the hovered column brightens, so the
     tip is visibly attached to a bar rather than floating over the plot. */
  .col:hover .seg {
    filter: brightness(1.18);
  }

  /*
   * The hover tip, mirroring `UserActivityChart`'s: CSS-only, opacity-toggled,
   * pointer-events:none. Deliberately NOT JS hover state — this chart renders
   * inside panes that can be `display: none`, and this project has already been
   * bitten by charts that measure or subscribe on mount and then render nothing
   * there. A rule that only fires on :hover cannot have that failure mode.
   */
  .tip {
    position: absolute;
    inset-inline-start: 50%;
    transform: translateX(-50%);
    padding: 6px 9px;
    background: var(--surface-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    box-shadow: var(--shadow);
    font-size: 11px;
    color: var(--text);
    white-space: nowrap;
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
    z-index: 3;
  }

  .col:hover .tip {
    opacity: 1;
  }

  .tip-date {
    color: var(--text-muted);
    margin-bottom: 4px;
  }

  .tip-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .tip-swatch {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    flex: 0 0 8px;
  }

  .tip-label {
    color: var(--text-muted);
    /* Pushes the value to the far edge so the numbers form a column that can
       be compared down the tip rather than read one by one. */
    margin-inline-end: auto;
  }

  .tip-value {
    font-variant-numeric: tabular-nums;
  }

  .tip-total {
    margin-top: 4px;
    padding-top: 4px;
    border-top: 1px solid var(--border);
  }

  .labels {
    display: flex;
    gap: 4px;
    margin-top: 4px;
    padding-inline-start: var(--yaxis-width);
  }

  .label-slot {
    flex: 1 1 0;
    min-width: 24px;
    text-align: center;
    font-size: 0.6875rem;
    color: var(--muted-fg);
    white-space: nowrap;
    overflow: visible;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin: 10px 0 0;
    padding: 0;
    list-style: none;
    font-size: 0.75rem;
    color: var(--muted-fg);
  }

  .legend li {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
    display: inline-block;
  }

  .dormant-swatch {
    background: var(--danger);
    opacity: 0.55;
  }
</style>
