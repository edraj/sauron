<script lang="ts">
  /**
   * The day behind one bar of the Performance page's latency/throughput charts.
   *
   * Those charts bucket by HOUR but label their axis by day, so a bar answers
   * "how did this hour compare" while the axis invites "how did this day go".
   * This is that second question: the clicked bar's local day, all 24 hours.
   *
   * It needs no request. The page already holds the hourly series the bars are
   * drawn from, so the day is a slice of data in memory — the modal opens on
   * the click with no spinner and no chance of showing a window that disagrees
   * with the chart behind it.
   */
  import { t } from '../i18n';
  import { localeStore } from '../i18n/locale.svelte';
  import Modal from './ui/Modal.svelte';
  import HourlyLineChart from './HourlyLineChart.svelte';
  import { sliceLocalDay } from '../models/day-detail';
  import { formatMs } from '../utils/format';
  import { formatNumber } from '../i18n';
  import type { PerfSeriesPoint } from '../models';

  type Metric = 'latency' | 'throughput';

  interface Props {
    open: boolean;
    /** The clicked bar's bucket; `null` whenever the modal is shut. */
    bucket: string | null;
    /** Which chart was clicked — that metric leads and owns the left axis. */
    metric: Metric;
    /** The page's hourly series, sliced here rather than refetched. */
    series: PerfSeriesPoint[];
    onclose?: () => void;
  }

  let { open = $bindable(false), bucket, metric, series, onclose }: Props = $props();

  let showSecondary = $state(false);

  // Each opening starts with only the clicked metric. Without this the toggle
  // would carry over from the last bar, so a Latency bar could open already
  // showing Throughput — a state the user never asked for on this bar.
  $effect(() => {
    bucket;
    metric;
    showSecondary = false;
  });

  const hours = $derived(bucket ? sliceLocalDay(series, bucket) : []);

  const dayLabel = $derived.by(() => {
    if (!bucket) return '';
    const d = new Date(bucket);
    if (Number.isNaN(d.getTime())) return bucket;
    return d.toLocaleDateString(localeStore.tag, {
      weekday: 'long',
      month: 'short',
      day: 'numeric',
      year: 'numeric',
    });
  });

  const measured = $derived(hours.filter((h) => h.latency !== null));
  const dayTransactions = $derived(hours.reduce((sum, h) => sum + h.throughput, 0));
  const peakLatency = $derived(
    measured.length ? Math.max(...measured.map((h) => h.latency as number)) : null,
  );
  const busiest = $derived.by(() => {
    if (!hours.length) return null;
    return hours.reduce((best, h) => (h.throughput > best.throughput ? h : best), hours[0]);
  });

  const secondaryLabel = $derived(
    metric === 'latency' ? t('perf.day.alsoThroughput') : t('perf.day.alsoLatency'),
  );
</script>

<Modal bind:open title={dayLabel} size="lg" {onclose}>
  {#snippet children()}
    <div class="head">
      <p class="muted lede">{t('perf.day.lede')}</p>
      <button
        type="button"
        class="toggle"
        class:on={showSecondary}
        aria-pressed={showSecondary}
        onclick={() => (showSecondary = !showSecondary)}
      >
        <span
          class="swatch"
          style="background:{metric === 'latency' ? 'var(--primary)' : 'var(--warning)'}"
        ></span>
        {secondaryLabel}
      </button>
    </div>

    <HourlyLineChart {hours} primary={metric} {showSecondary} />

    <dl class="facts">
      <div>
        <dt>{t('perf.day.transactions')}</dt>
        <dd>{formatNumber(dayTransactions)}</dd>
      </div>
      <div>
        <dt>{t('perf.day.peakP95')}</dt>
        <dd>{peakLatency === null ? '—' : formatMs(peakLatency)}</dd>
      </div>
      <div>
        <dt>{t('perf.day.busiestHour')}</dt>
        <dd>
          {busiest && busiest.throughput > 0
            ? `${String(busiest.hour).padStart(2, '0')}:00`
            : '—'}
        </dd>
      </div>
      <div>
        <dt>{t('perf.day.hoursWithTraffic')}</dt>
        <dd>{formatNumber(measured.length)} / 24</dd>
      </div>
    </dl>
  {/snippet}
</Modal>

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 4px;
  }
  .lede {
    font-size: 13px;
    margin: 0;
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    font: inherit;
    font-size: 12.5px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: background 0.12s ease, color 0.12s ease, border-color 0.12s ease;
  }
  .toggle:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .toggle.on {
    color: var(--text);
    background: var(--surface-3);
    border-color: var(--border-strong);
  }
  .swatch {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    opacity: 0.35;
    transition: opacity 0.12s ease;
  }
  .toggle.on .swatch {
    opacity: 1;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 12px 20px;
    margin: 18px 0 0;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .facts div {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .facts dt {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .facts dd {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
</style>
