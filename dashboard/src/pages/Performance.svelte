<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import { rangeKey, toParams, type DateRangeValue } from '../lib/models/date-range';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
  import { refreshRollups } from '../lib/api/rollups';
  import { approx } from '../lib/models/freshness';
  import { rollupState } from '../lib/stores/rollups.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import OperationTransactionsModal from '../lib/components/OperationTransactionsModal.svelte';
  import PerfDayModal from '../lib/components/PerfDayModal.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { perfSummary, perfSeries } from '../lib/api/performance';
  import { compactNumber, formatMs, formatPercent, latencyTone } from '../lib/utils/format';
  import { OPERATION_DEFAULT_SORT, operationAccessor } from '../lib/models/performance-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import type { PerfSummaryRow, PerfSeriesPoint } from '../lib/models';

  type BadgeTone = 'neutral' | 'primary' | 'error' | 'warning' | 'success' | 'info' | 'fatal';

  const OPS = ['All', 'navigation', 'http', 'screen_load', 'resource', 'custom'] as const;

  // The shared selection, falling back to this page's own 7 days until the
  // user has chosen one — see `stores/range.svelte.ts`.
  let range = $state<DateRangeValue>(rangeStore.effective(7));
  let op = $state<string>('All');

  /**
   * The table and both charts are one payload, not two: they share the same
   * inputs, the same request round, and — in the pre-cache code — the same
   * loading/error flags, with a failure clearing both. Caching them as one entry
   * keeps that all-or-nothing semantics exactly; two views would let the summary
   * render beside a chart from a failed fetch.
   */
  interface PerfPayload {
    rows: PerfSummaryRow[];
    series: PerfSeriesPoint[];
  }

  const EMPTY: PerfPayload = { rows: [], series: [] };

  // Cached view (lib/stores/cached-view.svelte.ts): cached rows paint instantly on
  // return, then refresh behind the button's spinner. Re-exposed under the names
  // the template already used, so the markup is unchanged.
  const view = new CachedView<PerfPayload>();

  const payload = $derived(view.data ?? EMPTY);
  const rows = $derived(payload.rows);
  const series = $derived(payload.series);
  const loading = $derived(view.loading);
  const revalidating = $derived(view.revalidating);
  const error = $derived(view.error);

  let refreshing = $state(false);

  /**
   * `force` bypasses the fresh-window short-circuit: Refresh and Retry both mean
   * "go to the network now".
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's transactions are served as another's.
   */
  async function load(appId: string, win: DateRangeValue, opv: string, force = false) {
    const opParam = opv === 'All' ? undefined : opv;
    await view.load(
      viewKey('performance.summary', appId, sessionStore.scopeKey, rangeKey(win), opParam),
      async () => {
        const w = toParams(win);
        const [summary, ser] = await Promise.all([
          perfSummary(appId, { ...w, op: opParam }),
          perfSeries(appId, { ...w, op: opParam }),
        ]);
        return { rows: summary, series: ser };
      },
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const win = range;
    const opv = op;
    if (aid) void load(aid, win, opv);
  });

  // The table arrives whole — `performance_summary` returns its top 100
  // operations in one response — so the sort runs over the ENTIRE array the
  // table renders. No pager: what is on screen is already all of it, and the
  // tiles above are computed from the same rows.
  //
  // A bare `SortState`, not the `OffsetListState` the paginated tables use:
  // that type exists to make "apply a sort" and "reset to page 1" one
  // indivisible step, and with no offset there is nothing to reset. Its
  // `key`/`dir` are `readonly` (see `sort.ts`), so `sort.dir = 'asc'` is a
  // type error and every transition goes through `toggleSort`.
  //
  // The seed reproduces the endpoint's own `ORDER BY count DESC`, so the page
  // opens exactly as it did and the caret names the column that produced it.
  let sort = $state<SortState>(OPERATION_DEFAULT_SORT);

  // `sortRows` copies before sorting. That is load-bearing, not tidy: `rows`
  // is the VERY ARRAY inside the view cache's payload, handed back by
  // reference (`cached-view.svelte.ts` says so, and `$state.raw` keeps that
  // identity exact), so an in-place sort would reorder the cached payload for
  // every later reader and the ordering would survive into the next visit to
  // this page. No runes machinery prevents that — only copying does.
  const sortedRows = $derived(sortRows(rows, operationAccessor(sort.key), sort.dir));

  function onsort(key: string, columnDefault: SortDir) {
    sort = toggleSort(sort, key, columnDefault);
  }

  // --- summary aggregates (client-side across the returned rows) -------------
  const throughput = $derived(rows.reduce((sum, r) => sum + r.count, 0));
  const maxP95 = $derived(rows.length ? Math.max(...rows.map((r) => r.p95)) : 0);
  const errorRate = $derived.by(() => {
    const total = rows.reduce((sum, r) => sum + r.count, 0);
    if (!total) return 0;
    return rows.reduce((sum, r) => sum + r.error_rate * r.count, 0) / total;
  });

  // --- series mapped for the bar charts --------------------------------------
  const latencyData = $derived(series.map((p) => ({ bucket: p.bucket, count: Math.round(p.p95) })));
  const throughputData = $derived(
    series.map((p) => ({ bucket: p.bucket, count: Math.round(p.throughput) })),
  );

  function opTone(o: string): BadgeTone {
    switch (o) {
      case 'navigation':
        return 'primary';
      case 'http':
        return 'info';
      case 'screen_load':
        return 'success';
      case 'resource':
        return 'neutral';
      case 'custom':
        return 'warning';
      default:
        return 'neutral';
    }
  }

  function opLabel(o: string): string {
    return o === 'All' ? 'All' : o.replace('_', ' ');
  }

  // --- drill-down ------------------------------------------------------------
  //
  // The table is an aggregate over `(name, op)`; this is the row's individual
  // spans. Held as the ROW, not as a name string, because the modal filters on
  // both halves of the group key — two rows can share a name under different
  // ops, and a name-only drill-down would show more spans than the row counted.
  //
  // `null` whenever the modal is shut, so a reopen cannot flash the previous
  // operation's title while the new request is in flight.
  let selected = $state<PerfSummaryRow | null>(null);
  let drillOpen = $state(false);

  function openDrill(r: PerfSummaryRow) {
    selected = r;
    drillOpen = true;
  }

  function closeDrill() {
    drillOpen = false;
    selected = null;
  }

  // --- day detail ------------------------------------------------------------
  //
  // A bar on either chart above is an HOUR, while the axis beneath it is
  // labelled by day — so the charts answer "how was this hour" under a legend
  // that invites "how was this day". This opens that second question: the
  // clicked bar's local day, hour by hour.
  //
  // The bucket, not the whole point: the modal slices `series` itself, and
  // handing it a `{bucket, count}` pair from the mapped chart data would give
  // it the one metric that chart happens to plot, when the day it draws needs
  // both.
  let dayBucket = $state<string | null>(null);
  let dayMetric = $state<'latency' | 'throughput'>('latency');
  let dayOpen = $state(false);

  function openDay(bucket: string, metric: 'latency' | 'throughput') {
    dayBucket = bucket;
    dayMetric = metric;
    dayOpen = true;
  }

  function closeDay() {
    dayOpen = false;
    dayBucket = null;
  }

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid, range, op, true);
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      // Kick an immediate rollup fold first (bounded server-side wait), so
      // the reloads below fetch aggregates that include the newest events.
      // Older APIs 404 this — then the reload alone is the refresh.
      await refreshRollups(aid).catch(() => {});
      // force: an explicit click must reach the network regardless of freshness.
      await load(aid, range, op, true);
    } finally {
      refreshing = false;
    }
  }
</script>

  <div class="head">
    <div>
      <h1 class="page-title">{t('perf.title')}</h1>
      <p class="muted sub">
        {t('perf.subtitle')}
      </p>
    </div>
    <div class="controls">
      <div class="ops" role="tablist" aria-label={t('perf.operationFilter')}>
        {#each OPS as o (o)}
          <button
            class="op"
            class:active={op === o}
            onclick={() => (op = o)}
            type="button"
            role="tab"
            aria-selected={op === o}
          >
            {opLabel(o)}
          </button>
        {/each}
      </div>
      <DateRange
        value={range}
        onchange={(v) => {
          range = v;
          rangeStore.set(v);
        }}
      />
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached data, fetching fresh" hint.
      -->
      <RollupChip />
      <RefreshButton
        onclick={refresh}
        loading={refreshing || revalidating}
        title={revalidating ? 'Refreshing…' : 'Refresh'}
      />
    </div>
  </div>

  {#if loading && rows.length === 0}
    <Skeleton rows={6} />
  {:else if error && rows.length === 0}
    <Card>
      <EmptyState title={t('perf.error.load')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>{t('common.retry')}</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title={t('perf.empty.title')}
        description={t('perf.empty.body')}
        icon="zap"
      />
    </Card>
  {:else}
    <!--
      No dim-and-disable while refreshing any more: under stale-while-revalidate
      `loading` is only ever true with nothing to show, so this branch never saw
      it, and re-binding it to `revalidating` would grey the page out and swallow
      clicks on every background refresh. The button's spinner is the indicator.
    -->
    <div class="body">
      <StatTiles min={170}>
        <StatTile label={t('perf.stat.throughput')} value={compactNumber(throughput)} sub="transactions" />
        <StatTile label={t('perf.card.operations')} value={rows.length} sub="tracked" />
        <StatTile
          label={t('perf.stat.p95')}
          value={approx(formatMs(maxP95), rollupState.ready)}
          sub="slowest operation"
          tone={latencyTone(maxP95)}
        />
        <StatTile
          label={t('perf.stat.errorRate')}
          value={formatPercent(errorRate)}
          sub="weighted by volume"
          tone={errorRate > 0.01 ? 'error' : 'success'}
        />
      </StatTiles>

      <div class="charts">
        <Card>
          {#snippet header()}
            <div class="chart-head">
              <h3 class="ch-title">{t('perf.card.latencyOverTime')}</h3>
              <span class="caption">p95 latency (ms)</span>
            </div>
          {/snippet}
          <TimeSeriesChart
            data={latencyData}
            height={200}
            color="var(--warning)"
            onselect={(p) => openDay(p.bucket, 'latency')}
          />
        </Card>

        <Card>
          {#snippet header()}
            <div class="chart-head">
              <h3 class="ch-title">{t('perf.card.throughputOverTime')}</h3>
              <span class="caption">{t('prose.perf.perBucket')}</span>
            </div>
          {/snippet}
          <TimeSeriesChart
            data={throughputData}
            height={200}
            color="var(--primary)"
            onselect={(p) => openDay(p.bucket, 'throughput')}
          />
        </Card>
      </div>

      <Card title={t('perf.card.operations')} padding="none" class="ops-card">
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="name" columnDefault="asc" {sort} {onsort}>{t('perf.column.name')}</SortableTh>
              <SortableTh key="op" columnDefault="asc" {sort} {onsort}>Op</SortableTh>
              <SortableTh key="throughput" class="num" {sort} {onsort}>{t('perf.stat.throughput')}</SortableTh>
              <SortableTh key="p50" class="num" {sort} {onsort}>p50</SortableTh>
              <SortableTh key="p95" class="num" {sort} {onsort}>p95</SortableTh>
              <SortableTh key="p99" class="num" {sort} {onsort}>p99</SortableTh>
              <SortableTh key="avg" class="num" {sort} {onsort}>{t('perf.column.avg')}</SortableTh>
              <SortableTh key="error_rate" class="num" {sort} {onsort}>{t('perf.stat.errorRate')}</SortableTh>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each sortedRows as r (r.op + '::' + r.name)}
              <!--
                The whole row opens the drill-down, not just the name cell: every
                number in the row provokes the same question ("which spans made
                that p99?"), so every cell is a reasonable place to click. The
                name still carries the link colouring, because a row of plain
                text with a pointer cursor advertises nothing.
              -->
              <tr class="clickable" onclick={() => openDrill(r)}>
                <td>
                  <span class="name mono truncate" title={r.name}>{r.name}</span>
                </td>
                <td>
                  <Badge tone={opTone(r.op)} size="sm">{opLabel(r.op)}</Badge>
                </td>
                <td class="num">{formatNumber(r.count)}</td>
                <td class="num">{#if rollupState.ready}<span class="approx-mark">≈</span>{/if}<LatencyBadge ms={r.p50} size="sm" /></td>
                <td class="num">{#if rollupState.ready}<span class="approx-mark">≈</span>{/if}<LatencyBadge ms={r.p95} size="sm" /></td>
                <td class="num">{#if rollupState.ready}<span class="approx-mark">≈</span>{/if}<LatencyBadge ms={r.p99} size="sm" /></td>
                <td class="num">{#if rollupState.ready}<span class="approx-mark">≈</span>{/if}<LatencyBadge ms={r.avg} size="sm" /></td>
                <td class="num">
                  <span class="err-rate" class:err={r.error_rate > 0.01}>
                    {formatPercent(r.error_rate)}
                  </span>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>
      </Card>
    </div>
  {/if}

  <!--
    Mounted once, outside the loading/error branches, and fed the clicked row.
    Inside the `{#each}` it would be 100 dialogs; inside the `{:else}` branch a
    background refresh that emptied `rows` would unmount it mid-read.
  -->
  <OperationTransactionsModal
    bind:open={drillOpen}
    row={selected}
    appId={sessionStore.currentAppId}
    {range}
    onclose={closeDrill}
  />

  <!-- Mounted alongside the drill-down and for the same reason: inside the
       `{:else}` branch a background refresh that emptied `rows` would unmount
       it mid-read. -->
  <PerfDayModal
    bind:open={dayOpen}
    bucket={dayBucket}
    metric={dayMetric}
    {series}
    onclose={closeDay}
  />

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .ops {
    display: inline-flex;
    gap: 2px;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .op {
    padding: 6px 11px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    border-radius: var(--radius-sm);
    text-transform: capitalize;
    white-space: nowrap;
  }
  .op:hover {
    color: var(--text);
  }
  .op.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .charts {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
    align-items: start;
  }
  .chart-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
  }
  .ch-title {
    font-size: 14.5px;
    font-weight: 620;
  }
  .caption {
    font-size: 12px;
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }
  .name {
    display: inline-block;
    max-width: 340px;
    vertical-align: bottom;
  }
  /* The row's affordance. `tr.clickable` gives the pointer and the hover
     background, but a table of plain text still reads as inert — the colour
     shift on the name is what says the row goes somewhere. Scoped to a hover
     on the ROW so it tracks the real click target, not just this cell.
     `:global` is required: `tr.clickable` lives in DataTable's markup, so
     Svelte's scoped-CSS pass sees no `.clickable` in THIS component's template
     and would prune the selector outright. */
  :global(tr.clickable:hover) .name {
    color: var(--primary);
  }
  .err-rate {
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }
  .err-rate.err {
    color: var(--error);
    font-weight: 600;
  }

  @media (max-width: 900px) {
    .charts {
      grid-template-columns: 1fr;
    }
  }
</style>
