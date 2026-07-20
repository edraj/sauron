<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getOverview } from '../lib/api/overview';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber, formatPercent } from '../lib/utils/format';
  import type { Overview } from '../lib/models';

  const RANGES = [
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  let sinceDays = $state(30);
  let overview = $state<Overview | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);

  async function load(appId: string, days: number) {
    loading = true;
    error = null;
    try {
      overview = await getOverview(appId, days);
    } catch (err) {
      error = errorMessage(err);
      overview = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void load(aid, days);
  });

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid, sinceDays);
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([load(aid, sinceDays)]);
    } finally {
      refreshing = false;
    }
  }

  // Tone helpers — severity-driven coloring for the KPI row.
  const crashFreeTone = $derived.by(() => {
    const v = overview?.crash_free_sessions;
    if (v == null) return 'neutral';
    if (v >= 0.99) return 'success';
    if (v >= 0.95) return 'warning';
    return 'error';
  });

  const errorRateTone = $derived.by(() => {
    const v = overview?.error_rate;
    if (v == null) return 'neutral';
    if (v >= 0.05) return 'error';
    if (v >= 0.01) return 'warning';
    return 'success';
  });

  const newUserShare = $derived.by(() => {
    const t = overview?.totals;
    if (!t || t.users <= 0) return null;
    return t.new_users / t.users;
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Overview</h1>
      <p class="muted sub">Health and activity at a glance for the last {sinceDays} days.</p>
    </div>
    <div class="controls">
      <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} ranges={RANGES} />
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  {#if loading && !overview}
    <div class="center"><Spinner size={26} /></div>
  {:else if error && !overview}
    <Card>
      <EmptyState title="Couldn't load overview" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>Retry</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if overview}
    <StatTiles min={150}>
      <StatTile label="Events" value={compactNumber(overview.totals.events)} tone="primary" />
      <StatTile
        label="Errors"
        value={compactNumber(overview.totals.errors)}
        tone={overview.totals.errors > 0 ? 'error' : 'neutral'}
        sub={`${formatPercent(overview.error_rate)} error rate`}
      />
      <StatTile label="Sessions" value={compactNumber(overview.totals.sessions)} />
      <StatTile label="Users" value={compactNumber(overview.totals.users)} />
      <StatTile
        label="New users"
        value={compactNumber(overview.totals.new_users)}
        sub={newUserShare != null ? `${formatPercent(newUserShare)} of users` : undefined}
      />
      <StatTile
        label="Crash-free sessions"
        value={formatPercent(overview.crash_free_sessions)}
        tone={crashFreeTone}
        sub={`${compactNumber(overview.totals.crashed_sessions)} crashed`}
      />
      <StatTile
        label="Error rate"
        value={formatPercent(overview.error_rate)}
        tone={errorRateTone}
        sub="errors / events"
      />
    </StatTiles>

    <div class="grid">
      <div class="col">
        <Card title="Event volume">
          <TimeSeriesChart
            data={overview.events_series}
            height={220}
            color="var(--primary)"
            emptyLabel="No events in this range"
          />
        </Card>
        <Card title="Errors over time">
          <TimeSeriesChart
            data={overview.errors_series}
            height={180}
            color="var(--error)"
            emptyLabel="No errors in this range — nice."
          />
        </Card>
      </div>

      <div class="col">
        <Card title="Top issues" padding="sm">
          {#if overview.top_issues.length === 0}
            <EmptyState
              title="No issues"
              description="No errors have been grouped into issues yet."
              icon="check"
            />
          {:else}
            <div class="issues">
              {#each overview.top_issues as issue (issue.id)}
                <a class="issue-row" href={`#/issues/${issue.id}`}>
                  <span class="issue-title truncate">{issue.title}</span>
                  <LevelBadge level={issue.level} size="sm" />
                  <span class="issue-count mono" title="times seen">
                    {compactNumber(issue.times_seen)}
                  </span>
                </a>
              {/each}
            </div>
          {/if}
        </Card>

        <Card title="Top events">
          {#if overview.top_events.length === 0}
            <EmptyState
              title="No events"
              description="Send events from your SDK to see them here."
              icon="chart-column"
            />
          {:else}
            <BarList items={overview.top_events} />
          {/if}
        </Card>
      </div>
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
  .controls {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 320px;
  }
  .grid {
    display: grid;
    grid-template-columns: 1.5fr 1fr;
    gap: 18px;
    margin-top: 18px;
    align-items: start;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }
  .issues {
    display: flex;
    flex-direction: column;
  }
  .issue-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 8px;
    border-radius: var(--radius-sm);
    text-decoration: none;
    color: inherit;
    transition: background 0.12s ease;
  }
  .issue-row:hover {
    background: var(--surface-2);
  }
  .issue-row + .issue-row {
    border-top: 1px solid var(--border);
  }
  .issue-title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    color: var(--text);
  }
  .issue-count {
    flex-shrink: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
