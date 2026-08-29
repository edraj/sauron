<script lang="ts">
  import { t, formatNumber } from '../i18n';
  import { lifecycleBars, lifecycleScale } from '../models/retention';
  import type { LifecyclePoint } from '../models/retention';

  interface Props {
    points: LifecyclePoint[];
  }

  let { points }: Props = $props();

  const bars = $derived(lifecycleBars(points));
  const scale = $derived(lifecycleScale(bars));

  const SERIES = [
    { key: 'new' as const, label: 'retention.lifecycle.new', color: 'var(--primary)' },
    { key: 'returning' as const, label: 'retention.lifecycle.returning', color: 'var(--success)' },
    {
      key: 'resurrected' as const,
      label: 'retention.lifecycle.resurrected',
      color: 'var(--warning)',
    },
  ];

  /** Percentage of the half-height one value occupies. */
  function share(v: number): number {
    return (Math.abs(v) / scale) * 100;
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

  /** "08-22" — month-day is enough beside hover titles with the full date. */
  function shortDate(iso: string): string {
    return iso.slice(5);
  }
</script>

<!--
  New / returning / resurrected stacked above the axis, dormant below it.

  Sized entirely from props and percentages — never from getBoundingClientRect
  inside requestAnimationFrame. A chart that measures itself that way renders
  nothing forever when its pane is `display: none`, because rAF never fires
  there; this project has hit that already.
-->
<div class="lifecycle">
  <div class="rows">
    {#each bars as b (b.start)}
      <div class="col" title={b.start}>
        <div class="half up">
          {#each SERIES as s (s.key)}
            {@const v = b.positive.find((p) => p.key === s.key)?.value ?? 0}
            {#if v > 0}
              <div
                class="seg"
                data-series={s.key}
                data-sign="positive"
                style="height:{share(v)}%;background:{s.color}"
                title={`${t(s.label as never)}: ${formatNumber(v)}`}
              ></div>
            {/if}
          {/each}
        </div>
        <div class="axis"></div>
        <div class="half down">
          {#if b.dormant < 0}
            <div
              class="seg"
              data-series="dormant"
              data-sign="negative"
              style="height:{share(b.dormant)}%"
              title={`${t('retention.lifecycle.dormant')}: ${formatNumber(-b.dormant)}`}
            ></div>
          {/if}
        </div>
      </div>
    {/each}
  </div>

  <!-- The axis dates the bars. Without this a reader hovers every bar to learn
       which period it is — fine for one bar, useless for spotting "the drop
       started in the week of the 18th" at a glance. -->
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
  }

  .rows {
    display: flex;
    align-items: stretch;
    gap: 4px;
    height: 200px;
    overflow-x: auto;
  }

  .col {
    flex: 1 1 0;
    min-width: 14px;
    display: flex;
    flex-direction: column;
  }

  .half {
    flex: 1 1 50%;
    display: flex;
  }

  /* Above the axis the stack grows downward from the top edge, so the tallest
     segment sits against the axis rather than floating. */
  .up {
    flex-direction: column;
    justify-content: flex-end;
  }

  .down {
    flex-direction: column;
    justify-content: flex-start;
  }

  .axis {
    height: 1px;
    background: var(--border);
  }

  .seg {
    width: 100%;
  }

  .seg[data-series='dormant'] {
    background: var(--danger);
    opacity: 0.55;
  }

  .labels {
    display: flex;
    gap: 4px;
    margin-top: 4px;
  }

  .label-slot {
    flex: 1 1 0;
    min-width: 14px;
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
