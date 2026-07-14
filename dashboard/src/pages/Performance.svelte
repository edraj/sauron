<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { perfSummary, perfSeries } from '../lib/api/performance';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber, formatMs, formatPercent, latencyTone } from '../lib/utils/format';
  import type { PerfSummaryRow, PerfSeriesPoint } from '../lib/models';

  type BadgeTone = 'neutral' | 'primary' | 'error' | 'warning' | 'success' | 'info' | 'fatal';

  const OPS = ['All', 'navigation', 'http', 'screen_load', 'resource', 'custom'] as const;

  let sinceDays = $state(7);
  let op = $state<string>('All');

  let rows = $state<PerfSummaryRow[]>([]);
  let series = $state<PerfSeriesPoint[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(appId: string, days: number, opv: string) {
    loading = true;
    error = null;
    const opParam = opv === 'All' ? undefined : opv;
    try {
      const [summary, ser] = await Promise.all([
        perfSummary(appId, { since_days: days, op: opParam }),
        perfSeries(appId, { since_days: days, op: opParam }),
      ]);
      rows = summary;
      series = ser;
    } catch (err) {
      error = errorMessage(err);
      rows = [];
      series = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    const opv = op;
    if (aid) void load(aid, days, opv);
  });

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

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid, sinceDays, op);
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Performance</h1>
      <p class="muted sub">
        Application performance monitoring — latency, throughput, and error rates by operation.
      </p>
    </div>
    <div class="controls">
      <div class="ops" role="tablist" aria-label="Operation filter">
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
      <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
    </div>
  </div>

  {#if loading && rows.length === 0}
    <div class="center"><Spinner size={24} /></div>
  {:else if error && rows.length === 0}
    <Card>
      <EmptyState title="Couldn't load performance" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>Retry</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title="No performance data yet"
        description="Once your SDK sends transactions — navigations, HTTP calls, screen loads and custom spans — their latency and throughput will show up here."
        icon="zap"
      />
    </Card>
  {:else}
    <div class="body" class:reloading={loading}>
      <StatTiles min={170}>
        <StatTile label="Throughput" value={compactNumber(throughput)} sub="transactions" />
        <StatTile label="Operations" value={rows.length} sub="tracked" />
        <StatTile
          label="p95 latency"
          value={formatMs(maxP95)}
          sub="slowest operation"
          tone={latencyTone(maxP95)}
        />
        <StatTile
          label="Error rate"
          value={formatPercent(errorRate)}
          sub="weighted by volume"
          tone={errorRate > 0.01 ? 'error' : 'success'}
        />
      </StatTiles>

      <div class="charts">
        <Card>
          {#snippet header()}
            <div class="chart-head">
              <h3 class="ch-title">Latency over time</h3>
              <span class="caption">p95 latency (ms)</span>
            </div>
          {/snippet}
          <TimeSeriesChart data={latencyData} height={200} color="var(--warning)" />
        </Card>

        <Card>
          {#snippet header()}
            <div class="chart-head">
              <h3 class="ch-title">Throughput over time</h3>
              <span class="caption">transactions / bucket</span>
            </div>
          {/snippet}
          <TimeSeriesChart data={throughputData} height={200} color="var(--primary)" />
        </Card>
      </div>

      <Card title="Operations" padding="none" class="ops-card">
        <DataTable>
          {#snippet head()}
            <tr>
              <th>Name</th>
              <th>Op</th>
              <th class="num">Throughput</th>
              <th class="num">p50</th>
              <th class="num">p95</th>
              <th class="num">p99</th>
              <th class="num">Avg</th>
              <th class="num">Error rate</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each rows as r (r.op + '::' + r.name)}
              <tr>
                <td>
                  <span class="name mono truncate" title={r.name}>{r.name}</span>
                </td>
                <td>
                  <Badge tone={opTone(r.op)} size="sm">{opLabel(r.op)}</Badge>
                </td>
                <td class="num">{r.count.toLocaleString()}</td>
                <td class="num"><LatencyBadge ms={r.p50} size="sm" /></td>
                <td class="num"><LatencyBadge ms={r.p95} size="sm" /></td>
                <td class="num"><LatencyBadge ms={r.p99} size="sm" /></td>
                <td class="num"><LatencyBadge ms={r.avg} size="sm" /></td>
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
</AppShell>

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
  .center {
    display: grid;
    place-items: center;
    min-height: 320px;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 18px;
    transition: opacity 0.15s ease;
  }
  .body.reloading {
    opacity: 0.6;
    pointer-events: none;
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
