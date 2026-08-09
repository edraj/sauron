<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import {
    getOverviewTotals,
    getOverviewSeries,
    getOverviewTopIssues,
    getOverviewTopEvents,
    getActiveUsersSeries,
  } from '../lib/api/overview';
  import type {
    ActiveUsersSeries,
    OverviewTotalsSection,
    OverviewSeriesSection,
  } from '../lib/api/overview';
  import { compactNumber, formatPercent } from '../lib/utils/format';
  import type { Issue, TopEvent } from '../lib/models';

  const RANGES = [
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  let sinceDays = $state(30);

  // FOUR independent views, not one.
  //
  // `/overview` runs five aggregates sequentially on one server connection and
  // returns nothing until the last finishes, so its latency is their sum —
  // measured at ~165 ms + ~160 ms + ~180 ms + series on a 210k-event app, and it
  // scales with the data. The whole page waited on that.
  //
  // Each section now fetches its own endpoint, so the browser issues them in
  // parallel (wall clock is the MAX, not the sum) and each card paints the moment
  // its own answer lands. Sections are also cached and revalidated separately,
  // which is what lets a return visit show the KPI tiles instantly while only the
  // slow half refreshes.
  const totalsView = new CachedView<OverviewTotalsSection>();
  const seriesView = new CachedView<OverviewSeriesSection>();
  const issuesView = new CachedView<Issue[]>();
  const eventsView = new CachedView<TopEvent[]>();
  const activeUsersView = new CachedView<ActiveUsersSeries>();

  const totals = $derived(totalsView.data ?? null);
  const series = $derived(seriesView.data ?? null);
  const topIssues = $derived(issuesView.data ?? null);
  const topEvents = $derived(eventsView.data ?? null);
  const activeUsers = $derived(activeUsersView.data ?? null);

  /**
   * `{ day, count }` from the API remapped to the `{ bucket, count }` that
   * `TimeSeriesChart` takes. Two different wire shapes for the same idea, so the
   * translation is explicit here rather than hidden behind a cast that would
   * silently plot `undefined`.
   */
  const activeUsersChart = $derived(
    (activeUsers?.series ?? []).map((p) => ({ bucket: p.day, count: p.count })),
  );

  // `revalidating` is the OR across sections: the one refresh button speaks for
  // the whole page, and a spinner that stopped while a section was still fetching
  // would claim the page was settled when it was not.
  const revalidating = $derived(
    totalsView.revalidating ||
      seriesView.revalidating ||
      issuesView.revalidating ||
      eventsView.revalidating ||
      activeUsersView.revalidating,
  );

  /**
   * Top issues additionally needs `issue:read`, and the endpoint answers 403
   * rather than an empty list so the card can be HIDDEN instead of showing a
   * reassuring "No issues" to someone who simply cannot see them.
   *
   * Reads the status, not the prose. This used to be
   * `/forbidden|permission|403/i.test(issuesView.error)`, which is wrong in both
   * directions: an unrelated failure whose message merely contains the word
   * "permission" would hide the card, and a 403 phrased without any of those
   * words would leave it showing an error. `CachedView.errorStatus` now carries
   * the code, so the check can be exact.
   */
  const issuesForbidden = $derived(issuesView.errorStatus === 403);

  let refreshing = $state(false);

  /**
   * `force` bypasses the fresh-window short-circuit: Refresh and Retry both mean
   * "go to the network now".
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's overview is served as another's.
   */
  async function load(appId: string, days: number, force = false) {
    const scope = sessionStore.scopeKey;
    // Started together and NOT awaited in sequence: awaiting them one after
    // another here would rebuild exactly the sum-of-latencies the split exists to
    // remove. `allSettled`, not `all` — each section reports its own failure
    // through its own view, and one 403 or timeout must not abort the others.
    await Promise.allSettled([
      totalsView.load(
        viewKey('overview.totals', appId, scope, days),
        () => getOverviewTotals(appId, days),
        force,
      ),
      seriesView.load(
        viewKey('overview.series', appId, scope, days),
        () => getOverviewSeries(appId, days),
        force,
      ),
      issuesView.load(
        viewKey('overview.topIssues', appId, scope, days),
        () => getOverviewTopIssues(appId, days),
        force,
      ),
      eventsView.load(
        viewKey('overview.topEvents', appId, scope, days),
        () => getOverviewTopEvents(appId, days),
        force,
      ),
      activeUsersView.load(
        viewKey('overview.activeUsers', appId, scope, days),
        () => getActiveUsersSeries(appId, days),
        force,
      ),
    ]);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    if (aid) void load(aid, days);
  });

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid, sinceDays, true);
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      // force: an explicit click must reach the network regardless of freshness.
      await load(aid, sinceDays, true);
    } finally {
      refreshing = false;
    }
  }

  // Tone helpers — severity-driven coloring for the KPI row.
  const crashFreeTone = $derived.by(() => {
    const v = totals?.crash_free_sessions;
    if (v == null) return 'neutral';
    if (v >= 0.99) return 'success';
    if (v >= 0.95) return 'warning';
    return 'error';
  });

  const errorRateTone = $derived.by(() => {
    const v = totals?.error_rate;
    if (v == null) return 'neutral';
    if (v >= 0.05) return 'error';
    if (v >= 0.01) return 'warning';
    return 'success';
  });

  const newUserShare = $derived.by(() => {
    const t = totals?.totals;
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
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached data, fetching fresh" hint.
      -->
      <RefreshButton
        onclick={refresh}
        loading={refreshing || revalidating}
        title={revalidating ? 'Refreshing…' : 'Refresh'}
      />
    </div>
  </div>

  <!--
    No page-wide {#if loading} gate any more. That single condition is what made
    the whole page wait on the slowest query: every card was inside it. Each
    section now decides for itself whether it has data, is still arriving, or
    failed — so the KPI tiles appear while the charts are still loading, and a
    failing section degrades to its own message instead of blanking the page.
  -->
  {#if totals}
    <StatTiles min={150}>
      <StatTile label="Events" value={compactNumber(totals.totals.events)} tone="primary" />
      <StatTile
        label="Errors"
        value={compactNumber(totals.totals.errors)}
        tone={totals.totals.errors > 0 ? 'error' : 'neutral'}
        sub={`${formatPercent(totals.error_rate)} error rate`}
      />
      <StatTile label="Sessions" value={compactNumber(totals.totals.sessions)} />
      <StatTile label="Users" value={compactNumber(totals.totals.users)} />
      <StatTile
        label="New users"
        value={compactNumber(totals.totals.new_users)}
        sub={newUserShare != null ? `${formatPercent(newUserShare)} of users` : undefined}
      />
      <StatTile
        label="Crash-free sessions"
        value={formatPercent(totals.crash_free_sessions)}
        tone={crashFreeTone}
        sub={`${compactNumber(totals.totals.crashed_sessions)} crashed`}
      />
      <StatTile
        label="Error rate"
        value={formatPercent(totals.error_rate)}
        tone={errorRateTone}
        sub="errors / events"
      />
    </StatTiles>
  {:else if totalsView.error}
    <Card>
      <EmptyState title="Couldn't load totals" description={totalsView.error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>Retry</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <!-- Tile-height rows, so the KPI strip does not jump when it fills in. -->
    <Card><Skeleton rows={2} height="34px" label="Loading totals" /></Card>
  {/if}

  <div class="grid">
    <div class="col">
      <Card title="Event volume">
        {#if series}
          <TimeSeriesChart
            data={series.events_series}
            height={220}
            color="var(--primary)"
            emptyLabel="No events in this range"
          />
        {:else if seriesView.error}
          <EmptyState title="Couldn't load chart" description={seriesView.error} icon="triangle-alert" />
        {:else}
          <Skeleton rows={1} height="220px" label="Loading event volume" />
        {/if}
      </Card>
      <Card title="Errors over time">
        {#if series}
          <TimeSeriesChart
            data={series.errors_series}
            height={180}
            color="var(--error)"
            emptyLabel="No errors in this range — nice."
          />
        {:else if seriesView.error}
          <EmptyState title="Couldn't load chart" description={seriesView.error} icon="triangle-alert" />
        {:else}
          <Skeleton rows={1} height="180px" label="Loading errors over time" />
        {/if}
      </Card>
      <Card title="Active users">
        {#if activeUsers}
          <TimeSeriesChart
            data={activeUsersChart}
            height={180}
            color="var(--success, var(--primary))"
            emptyLabel="No identified activity in this range"
          />
          {#if activeUsers.partial_days.length > 0}
            <!--
              Named, not silently dropped. A day whose distinct count cannot be
              computed exactly is left OUT of the series, and an unexplained gap
              in a chart reads as an outage. Empty in the default day-granular
              configuration — see `tier_read::active_users_by_day`.
            -->
            <p class="partial">
              {activeUsers.partial_days.length} day{activeUsers.partial_days.length > 1 ? 's' : ''}
              omitted: {activeUsers.partial_days[0].reason}
            </p>
          {/if}
        {:else if activeUsersView.error}
          <EmptyState
            title="Couldn't load active users"
            description={activeUsersView.error}
            icon="triangle-alert"
          />
        {:else}
          <Skeleton rows={1} height="180px" label="Loading active users" />
        {/if}
      </Card>
    </div>

    <div class="col">
      <!--
        Hidden entirely, not shown empty, when the caller lacks `issue:read`: an
        empty "No issues" card would read as good news rather than as data the
        viewer is not permitted to see.
      -->
      {#if !issuesForbidden}
        <Card title="Top issues" padding="sm">
          {#if topIssues}
            {#if topIssues.length === 0}
              <EmptyState
                title="No issues"
                description="No errors have been grouped into issues yet."
                icon="check"
              />
            {:else}
              <div class="issues">
                {#each topIssues as issue (issue.id)}
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
          {:else if issuesView.error}
            <EmptyState title="Couldn't load issues" description={issuesView.error} icon="triangle-alert" />
          {:else}
            <Skeleton rows={5} label="Loading top issues" />
          {/if}
        </Card>
      {/if}

      <Card title="Top events">
        {#if topEvents}
          {#if topEvents.length === 0}
            <EmptyState
              title="No events"
              description="Send events from your SDK to see them here."
              icon="chart-column"
            />
          {:else}
            <BarList items={topEvents} />
          {/if}
        {:else if eventsView.error}
          <EmptyState title="Couldn't load events" description={eventsView.error} icon="triangle-alert" />
        {:else}
          <Skeleton rows={5} label="Loading top events" />
        {/if}
      </Card>
    </div>
  </div>
</AppShell>

<style>
  /* Reason text under the active-users chart. Muted: it explains an absence, it
     is not itself a warning. */
  .partial {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--text-muted, #888);
  }
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
