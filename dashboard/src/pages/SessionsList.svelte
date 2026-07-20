<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import DurationHistogram from '../lib/components/DurationHistogram.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listSessions, getSessionAnalytics } from '../lib/api/sessions';
  import { errorMessage } from '../lib/api/client';
  import {
    relativeTime,
    formatDateTime,
    formatDuration,
    durationBetween,
    compactNumber,
  } from '../lib/utils/format';
  import type { Session, SessionsAnalytics, SeriesPoint } from '../lib/models';

  const LIMIT = 50;

  let sinceDays = $state(30);
  let offset = $state(0);
  let search = $state('');

  let sessions = $state<Session[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let analytics = $state<SessionsAnalytics | null>(null);
  let analyticsError = $state<string | null>(null);

  let refreshing = $state(false);

  // TimeSeriesChart consumes {bucket, count}; map avg_ms → count and format as duration.
  const durationSeries = $derived<SeriesPoint[]>(
    (analytics?.duration_series ?? []).map((p) => ({ bucket: p.bucket, count: p.avg_ms })),
  );

  async function loadAnalytics(appId: string, days: number) {
    analyticsError = null;
    try {
      analytics = await getSessionAnalytics(appId, days);
    } catch (err) {
      analyticsError = errorMessage(err);
      analytics = null;
    }
  }

  async function load(appId: string, days: number, off: number) {
    loading = true;
    error = null;
    try {
      sessions = await listSessions(appId, { since_days: days, limit: LIMIT, offset: off });
    } catch (err) {
      error = errorMessage(err);
      sessions = [];
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([load(aid, sinceDays, offset), loadAnalytics(aid, sinceDays)]);
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    const off = offset;
    if (aid) void load(aid, days, off);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void loadAnalytics(aid, days);
  });

  function onRange(days: number) {
    if (days === sinceDays) return;
    offset = 0;
    sinceDays = days;
  }

  // Client-side filter over the loaded page — matches session / user / device.
  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    if (!q) return sessions;
    return sessions.filter(
      (s) =>
        s.session_id.toLowerCase().includes(q) ||
        (s.distinct_id?.toLowerCase().includes(q) ?? false) ||
        (s.device_key?.toLowerCase().includes(q) ?? false),
    );
  });

  function openSession(id: string) {
    push('/sessions/' + encodeURIComponent(id));
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Sessions</h1>
      <p class="muted sub">User sessions — activity, duration and errors over time.</p>
    </div>
    <div class="controls">
      <SearchInput bind:value={search} placeholder="Filter session / user / device…" width="280px" />
      <DateRange value={sinceDays} onchange={onRange} />
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  <div class="analytics-head">
    <h2 class="section-title">Session engagement</h2>
    <DateRange value={sinceDays} onchange={onRange} />
  </div>

  {#if analytics}
    <StatTiles min={160}>
      <StatTile label="Sessions" value={compactNumber(analytics.stats.sessions)} tone="primary" sub={`last ${sinceDays}d`} />
      <StatTile label="Crashed" value={compactNumber(analytics.stats.crashed)} tone={analytics.stats.crashed > 0 ? 'warning' : 'neutral'} />
      <StatTile label="Avg session" value={formatDuration(analytics.stats.avg_session_ms)} />
      <StatTile label="Median session" value={formatDuration(analytics.stats.median_session_ms)} />
    </StatTiles>

    <div class="session-charts">
      <Card title="Average session duration per day">
        <TimeSeriesChart data={durationSeries} format={formatDuration} showTotal={false} />
      </Card>
      <Card title="Session length distribution">
        <DurationHistogram data={analytics.duration_histogram} />
      </Card>
    </div>
  {:else if analyticsError}
    <Card><p class="muted">{analyticsError}</p></Card>
  {/if}

  <Card padding="none">
    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if error}
      <EmptyState title="Couldn't load sessions" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() =>
              sessionStore.currentAppId && load(sessionStore.currentAppId, sinceDays, offset)}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    {:else if sessions.length === 0}
      <EmptyState
        title="No sessions yet"
        description="No sessions recorded in this range. Widen the date range or send activity from your SDK."
        icon="inbox"
      />
    {:else if filtered.length === 0}
      <EmptyState
        title="No matches"
        description={`No sessions on this page match “${search}”.`}
        icon="search"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th>Session</th>
            <th>User</th>
            <th>Device</th>
            <th>Started</th>
            <th>Duration</th>
            <th class="num">Events</th>
            <th class="num">Errors</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each filtered as s (s.id)}
            <tr class="clickable" onclick={() => openSession(s.session_id)}>
              <td><span class="mono sid" title={s.session_id}>{s.session_id}</span></td>
              <td>
                {#if s.distinct_id}
                  <a
                    class="link mono trunc"
                    href={`#/persons/${encodeURIComponent(s.distinct_id)}`}
                    onclick={(e) => e.stopPropagation()}
                    title={s.distinct_id}
                  >
                    {s.distinct_id}
                  </a>
                {:else}
                  <span class="muted">anonymous</span>
                {/if}
              </td>
              <td>
                {#if s.device_key}
                  <a
                    class="link mono trunc"
                    href={`#/devices/${encodeURIComponent(s.device_key)}`}
                    onclick={(e) => e.stopPropagation()}
                    title={s.device_key}
                  >
                    {s.device_key}
                  </a>
                {:else}
                  <span class="faint">—</span>
                {/if}
              </td>
              <td class="muted" title={formatDateTime(s.started_at)}>
                {relativeTime(s.started_at)}
              </td>
              <td class="muted">{formatDuration(durationBetween(s.started_at, s.last_event_at))}</td>
              <td class="num">{s.events_count.toLocaleString()}</td>
              <td class="num">
                <span class:err={s.errors_count > 0}>{s.errors_count.toLocaleString()}</span>
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
      <Pagination {offset} limit={LIMIT} count={sessions.length} onchange={(o) => (offset = o)} />
    {/if}
  </Card>
</AppShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
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
  .session-charts {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
    align-items: start;
    margin: 16px 0;
  }
  @media (max-width: 900px) {
    .session-charts {
      grid-template-columns: 1fr;
    }
  }
  .analytics-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin: 8px 0 12px;
  }
  .section-title {
    font-size: 15px;
    font-weight: 640;
    margin: 0;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 60px;
  }
  .sid {
    display: inline-block;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 12px;
  }
  .link {
    color: var(--text);
    text-decoration: none;
    transition: color 0.12s ease;
  }
  .link:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .trunc {
    display: inline-block;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 12px;
  }
  .err {
    color: var(--error);
    font-weight: 620;
  }
</style>
