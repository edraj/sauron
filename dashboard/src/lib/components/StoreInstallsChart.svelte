<script lang="ts">
  import { t } from '../i18n';
  import type { StoreDay } from '../api/stores';
  import { dayTotals, divergingScale } from './stores';
  import { utcDayLabel } from '../models/active-users';

  interface Props {
    data: StoreDay[];
    height?: number;
    emptyLabel?: string;
  }

  let {
    data,
    height = 200,
    emptyLabel = 'No store activity in this range',
  }: Props = $props();

  // ONE scale for both directions — see `divergingScale`. Each half owns half
  // the plot, so a bar's height is its share of the scale times 50%.
  const scale = $derived(divergingScale(data));

  function half(v: number): number {
    if (scale <= 0) return 0;
    // A zero keeps a 1% stub, matching the other charts: "a day that happened
    // and had none" must read differently from a gap in the data.
    return v === 0 ? 1 : Math.max(2, (v / scale) * 50);
  }

  /**
   * `utcDayLabel`, not `new Date(day)`: these are pure `YYYY-MM-DD` calendar
   * days. The default path parses as UTC and renders in the viewer's zone, so
   * west of Greenwich every bar is labelled with the previous day.
   */
  function label(day: string): string {
    return utcDayLabel(day);
  }

  function tip(d: StoreDay): string {
    const totals = dayTotals(d);
    const parts = [label(d.day)];
    if (d.google_play) {
      parts.push(`Play +${d.google_play.installs} / −${d.google_play.uninstalls}`);
    }
    if (d.app_store) {
      parts.push(`App Store +${d.app_store.installs} / −${d.app_store.uninstalls}`);
    }
    parts.push(`net ${totals.installs - totals.uninstalls >= 0 ? '+' : ''}${totals.installs - totals.uninstalls}`);
    return parts.join(' · ');
  }
</script>

{#if data.length === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px">
      <div class="bars">
        {#each data as point (point.day)}
          <div class="col" title={tip(point)}>
            <!-- Installs grow UP from the zero line, uninstalls DOWN. Each
                 stack is ordered Play then App Store so the two halves read as
                 mirror images of the same two colours. -->
            <div class="up">
              {#if point.app_store}
                <div
                  class="bar apple"
                  style="height:{half(point.app_store.installs)}%"
                ></div>
              {/if}
              {#if point.google_play}
                <div
                  class="bar play"
                  style="height:{half(point.google_play.installs)}%"
                ></div>
              {/if}
            </div>
            <div class="down">
              {#if point.google_play}
                <div
                  class="bar play dim"
                  style="height:{half(point.google_play.uninstalls)}%"
                ></div>
              {/if}
              {#if point.app_store}
                <div
                  class="bar apple dim"
                  style="height:{half(point.app_store.uninstalls)}%"
                ></div>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    </div>
    <div class="axis">
      <span>{label(data[0].day)}</span>
      <span class="legend">
        <i class="k play"></i> {t('ui.store.play')}
        <i class="k apple"></i> {t('ui.store.appStore')}
        <span class="dirs">{t('prose.store.directions')}</span>
      </span>
      <span>{label(data[data.length - 1].day)}</span>
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
    position: relative;
  }
  .bars {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: stretch;
    gap: 3px;
    padding: 4px 2px;
  }
  .col {
    position: relative;
    flex: 1;
    min-width: 3px;
    display: flex;
    flex-direction: column;
  }
  /* The zero line sits at the vertical midpoint; the two halves are equal
     boxes either side of it, which is what makes a shared scale legible. */
  .up {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    align-items: center;
    border-bottom: 1px solid var(--border);
  }
  .down {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: flex-start;
    align-items: center;
  }
  .bar {
    width: 100%;
    max-width: 42px;
    min-height: 1px;
    transition: filter 0.12s ease;
  }
  .up .bar:last-child {
    border-radius: 3px 3px 0 0;
  }
  .down .bar:last-child {
    border-radius: 0 0 3px 3px;
  }
  .bar.play {
    background: var(--primary);
  }
  .bar.apple {
    background: var(--info);
  }
  /* Uninstalls are the same two colours at lower emphasis: same store, other
     direction. A third and fourth hue would read as four unrelated series. */
  .bar.dim {
    opacity: 0.55;
  }
  .col:hover .bar {
    filter: brightness(1.18);
  }
  .axis {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    font-size: 11.5px;
    color: var(--text-muted);
  }
  .legend {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .legend .k {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 2px;
  }
  .legend .k.play {
    background: var(--primary);
  }
  .legend .k.apple {
    background: var(--info);
  }
  .dirs {
    margin-inline-start: 4px;
    opacity: 0.8;
  }
  .chart-empty {
    display: grid;
    place-items: center;
    font-size: 13px;
    color: var(--text-muted);
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
</style>
