<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import SearchAutocompleteInput from '../lib/components/search/SearchAutocompleteInput.svelte';
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import DurationHistogram from '../lib/components/DurationHistogram.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listSessions, getSessionAnalytics } from '../lib/api/sessions';
  import type { SearchEnvelope } from '../lib/api/search';
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';
  import { preflight, queryErrorFor } from '../lib/utils/query-error';
  import {
    setOffsetPage,
    setOffsetSort,
    type OffsetListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import {
    formatDuration,
    durationBetween,
    compactNumber,
  } from '../lib/utils/format';
  import type { Session, SessionsAnalytics, SeriesPoint } from '../lib/models';

  const LIMIT = 50;

  let sinceDays = $state(30);
  let search = $state('');

  /**
   * `started_at`, matching the endpoint's default — which slice 3 CHANGED from
   * `last_event_at`. Naming any other column here would put the caret on a
   * header the server did not order by.
   */
  let list = $state<OffsetListState>({ sort: { key: 'started_at', dir: 'desc' }, offset: 0 });

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  // Cached views (lib/stores/cached-view.svelte.ts): cached rows paint instantly on
  // return, then refresh behind a spinner. Re-exposed under the template's existing
  // names, so the markup is unchanged.
  const sessionsView = new CachedView<SearchEnvelope<Session>>();
  const analyticsView = new CachedView<SessionsAnalytics>();

  const sessions = $derived(sessionsView.data?.data ?? []);
  const total = $derived(sessionsView.data?.total ?? 0);
  // Infer hasNext from offset and total returned by the envelope
  const hasNext = $derived(list.offset + LIMIT < total);
  const loading = $derived(sessionsView.loading);
  const error = $derived(sessionsView.error);
  const revalidating = $derived(sessionsView.revalidating || analyticsView.revalidating);

  const analytics = $derived(analyticsView.data ?? null);
  const analyticsError = $derived(analyticsView.error);

  /** The planner's narrowing of the session window, if it bound. */
  const clamped = $derived(sessionsView.data?.clamped ?? null);

  /** The sessions schema, held only for `did you mean`. */
  let searchSchema = $state<SchemaDefinition | null>(null);
  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    fetchSchema(id, 'sessions')
      .then((s) => {
        if (!cancelled) searchSchema = s;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  /** A local parse problem wins — it means no request was worth issuing. */
  const searchError = $derived(
    preflight(search) ?? queryErrorFor(sessionsView.errorStatus, error, searchSchema),
  );

  let refreshing = $state(false);

  // TimeSeriesChart consumes {bucket, count}; map avg_ms → count and format as duration.
  const durationSeries = $derived<SeriesPoint[]>(
    (analytics?.duration_series ?? []).map((p) => ({ bucket: p.bucket, count: p.avg_ms })),
  );

  // `scopeKey` belongs in every key: it carries the selected environment, which
  // the axios interceptor adds to the request but which appears in none of these
  // arguments. Omit it and one environment's sessions are served as another's.
  async function loadAnalytics(appId: string, days: number, force = false) {
    await analyticsView.load(
      viewKey('sessions.analytics', appId, sessionStore.scopeKey, days),
      () => getSessionAnalytics(appId, days),
      force,
    );
  }

  // `sort` is in the key for the same reason `days` and `off` are: without it a
  // header click finds the previous ordering already cached under the same key
  // and repaints it with NO request on the wire, so the sort looks like it
  // silently did nothing.
  async function load(
    appId: string,
    days: number,
    sort: string,
    off: number,
    query: string,
    force = false,
  ) {
    await sessionsView.load(
      viewKey('sessions.list', appId, sessionStore.scopeKey, days, sort, off, LIMIT, query),
      () => listSessions(appId, { sinceDays: days, sort, limit: LIMIT, offset: off, query: query || undefined }),
      force,
    );
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      // force: an explicit click must reach the network regardless of freshness.
      await Promise.all([
        load(aid, sinceDays, sortParam(list.sort), list.offset, search, true),
        loadAnalytics(aid, sinceDays, true),
      ]);
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    const sort = sortParam(list.sort);
    const off = list.offset;
    const q = search;
    if (aid) void load(aid, days, sort, off, q);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    if (aid) void loadAnalytics(aid, days);
  });

  function onRange(days: number) {
    if (days === sinceDays) return;
    list = setOffsetPage(list, 0);
    sinceDays = days;
  }

  function openSession(id: string) {
    push('/sessions/' + encodeURIComponent(id));
  }

  function downloadSessionsCsv() {
    if (!sessions || sessions.length === 0) return;
    const header = ['Session', 'Started', 'Duration', 'Events', 'Errors'];
    const rows = sessions.map((s) => [
      s.session_id,
      new Date(s.started_at).toISOString(),
      formatDuration(durationBetween(s.started_at, s.last_event_at)),
      s.events_count.toString(),
      s.errors_count.toString(),
    ]);

    const csvContent = [header, ...rows].map((row) => row.join(',')).join('\n');
    const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `sessions.csv`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Sessions</h1>
      <p class="muted sub">User sessions — activity, duration and errors over time.</p>
    </div>
    <div class="controls">
      {#if sessionStore.currentAppId}
        <div style="width: 280px">
          <!--
            No hand-written placeholder. This one used to advertise `@tag=v1`,
            which sessions do not carry — the catalog declares no tag dimension
            for the resource and the lowerer refuses `Store::Tag` outright, so
            every query built from that hint came back a 400. The component
            derives its example from the schema it loaded instead.
          -->
          <SearchAutocompleteInput bind:value={search} appId={sessionStore.currentAppId} context="sessions" error={searchError} />
        </div>
      {/if}
      <DateRange value={sinceDays} onchange={onRange} />
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
      <Button
        variant="secondary"
        disabled={sessions.length === 0}
        onclick={downloadSessionsCsv}
        title="Download visible sessions as CSV"
      >
        <Icon name="download" size={15} />
        Export CSV
      </Button>
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

  <!-- Above the session rows, not above the engagement charts: it describes
       what the LIST leaves out, and the charts run their own query. -->
  <SearchDisclosure {clamped} />

  <Card padding="none">
    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if error}
      <EmptyState title="Couldn't load sessions" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() =>
              sessionStore.currentAppId &&
              load(
                sessionStore.currentAppId,
                sinceDays,
                sortParam(list.sort),
                list.offset,
                search,
                true,
              )}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    {:else if sessions.length === 0}
      <EmptyState
        title="No matches"
        description={search ? `No sessions match “${search}”.` : "No sessions recorded in this range. Widen the date range or send activity from your SDK."}
        icon="inbox"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <!-- Session stays a plain `<th>`: the endpoint's whitelist has no
                 session-id column, and an unlisted `sort=` is a 400 rather
                 than a silently ignored parameter. An unsorted column is
                 honest; a header that 400s the page is not. -->
            <th>Session</th>
            <SortableTh key="distinct_id" columnDefault="asc" sort={list.sort} {onsort}>
              User
            </SortableTh>
            <SortableTh key="device_key" columnDefault="asc" sort={list.sort} {onsort}>
              Device
            </SortableTh>
            <SortableTh key="started_at" sort={list.sort} {onsort}>Started</SortableTh>
            <!-- No stored duration: the server orders by `last_event_at -
                 started_at`, the same interval this column renders. -->
            <SortableTh key="duration_ms" sort={list.sort} {onsort}>Duration</SortableTh>
            <SortableTh key="events_count" class="num" sort={list.sort} {onsort}>
              Events
            </SortableTh>
            <SortableTh key="errors_count" class="num" sort={list.sort} {onsort}>
              Errors
            </SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each sessions as s (s.id)}
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
              <td><TimeValue value={s.started_at} /></td>
              <td class="muted">{formatDuration(durationBetween(s.started_at, s.last_event_at))}</td>
              <td class="num">{s.events_count.toLocaleString()}</td>
              <td class="num">
                <span class:err={s.errors_count > 0}>{s.errors_count.toLocaleString()}</span>
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
      <!-- `hasNext` is the client's `limit + 1` over-fetch probe, not an
           inference from the row count: a final page of exactly `LIMIT` rows
           used to offer a Next that led to an empty page.

           `count` stays the FETCHED page size, not `filtered.length`: the
           search box above filters this page in the browser only, so the
           pager's range describes the page the server sent. -->
      <Pagination
        offset={list.offset}
        limit={LIMIT}
        count={sessions.length}
        {hasNext}
        onchange={(o) => (list = setOffsetPage(list, o))}
      />
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
    /* Deliberately NOT `align-items: start`: the duration chart and the
       histogram have different natural heights, and letting each size to its
       own content leaves two cards of visibly different height sitting side by
       side. Stretching makes the pair read as one row. */
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
