<script lang="ts">
  /**
   * One day, hour by hour, as one or two lines.
   *
   * Opened from a bar on the Performance page's latency/throughput charts,
   * where a bar is an hour and the axis is labelled by day. This is the day
   * behind that bar: 24 local slots, whether or not traffic reached them.
   *
   * The two series never share a scale. Milliseconds and transaction counts
   * are different quantities, so one axis would make the comparison a
   * coincidence of magnitudes — a 400 ms hour sitting above a 380-transaction
   * hour says nothing. Latency reads against the left axis, throughput
   * against the right, and each is coloured to match its axis and the card it
   * was opened from.
   */
  import { formatNumber, t } from '../i18n';
  import { formatMs } from '../utils/format';
  import type { DayHour } from '../models/day-detail';

  type Metric = 'latency' | 'throughput';

  interface Props {
    hours: DayHour[];
    /** The metric whose bar was clicked. Always drawn, always the left axis. */
    primary: Metric;
    /** Draw the other metric too, against the right axis. */
    showSecondary?: boolean;
    height?: number;
  }

  let { hours, primary, showSecondary = false, height = 260 }: Props = $props();

  // A fixed drawing space scaled by the browser: the alternative, stretching
  // with `preserveAspectRatio="none"`, would distort every glyph on the axes.
  const W = 760;
  const H = 300;
  const PAD_L = 56;
  const PAD_R = 56;
  const PAD_T = 18;
  const PAD_B = 38;
  const PLOT_W = W - PAD_L - PAD_R;
  const PLOT_H = H - PAD_T - PAD_B;

  const LATENCY = 'var(--warning)';
  const THROUGHPUT = 'var(--primary)';

  const secondary = $derived<Metric>(primary === 'latency' ? 'throughput' : 'latency');

  function x(hour: number): number {
    return PAD_L + (hour / 23) * PLOT_W;
  }

  /** Ceiling for a series, never 0 — a flat-zero day still needs an axis. */
  function ceiling(values: number[]): number {
    return Math.max(1, ...values);
  }

  const latMax = $derived(
    ceiling(hours.filter((h) => h.latency !== null).map((h) => h.latency as number)),
  );
  const thrMax = $derived(ceiling(hours.map((h) => h.throughput)));

  function y(value: number, max: number): number {
    return PAD_T + PLOT_H - (value / max) * PLOT_H;
  }

  /**
   * Latency as one or more path segments.
   *
   * Segments, not a single path: an hour with no transaction measured no
   * latency, and joining across it would draw a line through a value that was
   * never recorded. The line simply stops and resumes.
   */
  const latencyPaths = $derived.by(() => {
    const out: string[] = [];
    let run: string[] = [];
    for (const h of hours) {
      if (h.latency === null) {
        if (run.length) out.push(run.join(' '));
        run = [];
        continue;
      }
      const px = x(h.hour).toFixed(1);
      const py = y(h.latency, latMax).toFixed(1);
      run.push(`${run.length ? 'L' : 'M'}${px},${py}`);
    }
    if (run.length) out.push(run.join(' '));
    return out;
  });

  /** Throughput as one unbroken path — every hour has a real count. */
  const throughputPath = $derived(
    hours
      .map((h, i) => `${i ? 'L' : 'M'}${x(h.hour).toFixed(1)},${y(h.throughput, thrMax).toFixed(1)}`)
      .join(' '),
  );

  const showLatency = $derived(primary === 'latency' || showSecondary);
  const showThroughput = $derived(primary === 'throughput' || showSecondary);

  /** Four gridlines, and the tick values for whichever axis owns each side. */
  const ticks = [0, 0.25, 0.5, 0.75, 1];
  const leftMax = $derived(primary === 'latency' ? latMax : thrMax);
  const rightMax = $derived(primary === 'latency' ? thrMax : latMax);

  function axisLabel(metric: Metric, fraction: number, max: number): string {
    // The baseline is the origin, not a duration: `formatMs(0)` renders
    // "<1 ms", which reads as a real (very fast) measurement sitting where the
    // axis zero should be.
    if (fraction === 0) return '0';
    const v = fraction * max;
    return metric === 'latency' ? formatMs(v) : formatNumber(Math.round(v));
  }

  // Every third hour, so 24 labels do not collide at narrow widths.
  const hourLabels = $derived(hours.filter((h) => h.hour % 3 === 0));

  let active = $state<number | null>(null);
  const hovered = $derived(active === null ? null : hours[active]);
</script>

<div class="wrap" style="height:{height}px">
  <svg
    viewBox="0 0 {W} {H}"
    preserveAspectRatio="xMidYMid meet"
    role="img"
    aria-label={t('perf.day.chartLabel')}
  >
    <!-- gridlines + the two value axes -->
    {#each ticks as frac (frac)}
      <line
        class="grid"
        x1={PAD_L}
        x2={W - PAD_R}
        y1={PAD_T + PLOT_H - frac * PLOT_H}
        y2={PAD_T + PLOT_H - frac * PLOT_H}
      />
      <text class="tick" x={PAD_L - 8} y={PAD_T + PLOT_H - frac * PLOT_H} text-anchor="end">
        {axisLabel(primary, frac, leftMax)}
      </text>
      {#if showSecondary}
        <text class="tick right" x={W - PAD_R + 8} y={PAD_T + PLOT_H - frac * PLOT_H}>
          {axisLabel(secondary, frac, rightMax)}
        </text>
      {/if}
    {/each}

    {#each hourLabels as h (h.hour)}
      <text class="hour" x={x(h.hour)} y={H - 14} text-anchor="middle">
        {String(h.hour).padStart(2, '0')}
      </text>
    {/each}

    {#if showThroughput}
      <path
        class="line"
        d={throughputPath}
        style:--line={THROUGHPUT}
        class:muted={primary !== 'throughput'}
      />
    {/if}
    {#if showLatency}
      {#each latencyPaths as seg, i (i)}
        <path class="line" d={seg} style:--line={LATENCY} class:muted={primary !== 'latency'} />
      {/each}
      <!-- A lone measured hour has no segment to draw, so mark it. -->
      {#each hours as h (h.hour)}
        {#if h.latency !== null && (hours[h.hour - 1]?.latency ?? null) === null && (hours[h.hour + 1]?.latency ?? null) === null}
          <circle class="lone" cx={x(h.hour)} cy={y(h.latency, latMax)} r="2.5" style:--line={LATENCY} />
        {/if}
      {/each}
    {/if}

    {#if hovered}
      <line class="guide" x1={x(hovered.hour)} x2={x(hovered.hour)} y1={PAD_T} y2={PAD_T + PLOT_H} />
      {#if showThroughput}
        <circle class="dot" cx={x(hovered.hour)} cy={y(hovered.throughput, thrMax)} style:--line={THROUGHPUT} />
      {/if}
      {#if showLatency && hovered.latency !== null}
        <circle class="dot" cx={x(hovered.hour)} cy={y(hovered.latency, latMax)} style:--line={LATENCY} />
      {/if}
    {/if}

    <!-- Hit targets last so they sit above the lines. -->
    {#each hours as h (h.hour)}
      <rect
        class="hit"
        x={PAD_L + (h.hour / 24) * PLOT_W - PLOT_W / 48}
        y={PAD_T}
        width={PLOT_W / 24}
        height={PLOT_H}
        role="presentation"
        onmouseenter={() => (active = h.hour)}
        onmouseleave={() => (active = null)}
      />
    {/each}
  </svg>

  {#if hovered}
    <div
      class="tip"
      class:flip={hovered.hour > 15}
      style="left:{(x(hovered.hour) / W) * 100}%"
    >
      <div class="tip-hour">{String(hovered.hour).padStart(2, '0')}:00</div>
      {#if showLatency}
        <div class="tip-row">
          <span class="swatch" style="background:{LATENCY}"></span>
          <span class="tip-label">{t('perf.day.latency')}</span>
          <span class="tip-value">
            {hovered.latency === null ? t('perf.day.noData') : formatMs(hovered.latency)}
          </span>
        </div>
      {/if}
      {#if showThroughput}
        <div class="tip-row">
          <span class="swatch" style="background:{THROUGHPUT}"></span>
          <span class="tip-label">{t('perf.day.throughput')}</span>
          <span class="tip-value">{formatNumber(hovered.throughput)}</span>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
    width: 100%;
  }
  svg {
    width: 100%;
    height: 100%;
    display: block;
    overflow: visible;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
  }
  .tick,
  .hour {
    fill: var(--text-faint);
    font-size: 11px;
    dominant-baseline: middle;
    font-variant-numeric: tabular-nums;
  }
  .hour {
    dominant-baseline: auto;
  }
  .line {
    fill: none;
    stroke: var(--line);
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  /* The metric that was not clicked reads as context, not as the subject. */
  .line.muted {
    stroke-width: 1.5;
    opacity: 0.62;
  }
  .lone {
    fill: var(--line);
  }
  .guide {
    stroke: var(--border-strong);
    stroke-width: 1;
    stroke-dasharray: 3 3;
  }
  .dot {
    r: 4;
    fill: var(--surface);
    stroke: var(--line);
    stroke-width: 2;
  }
  .hit {
    fill: transparent;
  }
  .tip {
    position: absolute;
    top: 8px;
    transform: translateX(-50%);
    background: var(--surface-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    box-shadow: var(--shadow);
    padding: 7px 10px;
    font-size: 11.5px;
    pointer-events: none;
    white-space: nowrap;
    min-width: 132px;
  }
  /* Late hours would push the tip past the right edge of the modal. */
  .tip.flip {
    transform: translateX(-100%) translateX(-10px);
  }
  .tip-hour {
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
    padding-bottom: 4px;
    margin-bottom: 5px;
    font-variant-numeric: tabular-nums;
  }
  .tip-row {
    display: flex;
    align-items: center;
    gap: 7px;
    line-height: 1.7;
  }
  .swatch {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    flex: none;
  }
  .tip-label {
    flex: 1;
    color: var(--text-muted);
  }
  .tip-value {
    font-weight: 600;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
</style>
