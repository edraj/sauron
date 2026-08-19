<script lang="ts">
  import { t, formatNumber } from '../lib/i18n';
  import { push, querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import TimeFilter from '../lib/components/TimeFilter.svelte';
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import {
    SESSION_FIELDS,
    encodeFilters,
    parseFilters,
    type Filter,
  } from '../lib/components/filters/filters';
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
    fromParams,
    toParams,
    toRecord,
    type TimeField,
    type TimeFilterState,
  } from '../lib/models/time-filter';
  import {
    formatDuration,
    durationBetween,
    compactNumber,
  } from '../lib/utils/format';
  import type { Session, SessionsAnalytics, SeriesPoint } from '../lib/models';

  const LIMIT = 50;

  /**
   * The columns this list can be windowed by.
   *
   * `last_event_at` is the DEFAULT because it is what this list has always
   * filtered on: the route asked `resolve_window` for `"started_at"` while
   * `session_search_base` filtered `sessions.last_event_at`, so the envelope
   * named one column and the predicate used the other. Defaulting to
   * `started_at` here would silently change which sessions an untouched page
   * returns — a bigger change than fixing the label.
   *
   * Surfaced as "Last activity": `sessions` has no `ended_at` column at all,
   * and duration is derived, so "Ended" would name something that does not
   * exist.
   *
   * Identity and label are separate. `TIME_FIELD_KEYS` is the wire identity and
   * stays a plain const, because `fromParams` below reads it once at init to
   * validate the URL's field parameter — a `$derived` read there would be a
   * stale capture, and the labels are irrelevant to that check. `TIME_FIELDS`
   * adds the translated labels and IS derived, so the picker follows a
   * language switch.
   */
  const TIME_FIELD_KEYS = ['last_event_at', 'started_at'] as const;
  const TIME_FIELD_LABEL_KEYS = {
    last_event_at: 'sessions.timeField.lastActivity',
    started_at: 'sessions.column.started',
  } as const;
  const STATIC_TIME_FIELDS: TimeField[] = TIME_FIELD_KEYS.map((key) => ({ key, label: key }));
  const TIME_FIELDS: TimeField[] = $derived(
    TIME_FIELD_KEYS.map((key) => ({ key, label: t(TIME_FIELD_LABEL_KEYS[key]) })),
  );
  const DEFAULT_TIME_FIELD = 'last_event_at';
  const DEFAULT_DAYS = 30;

  const initialQs = new URLSearchParams($querystring ?? '');
  let timeFilter = $state<TimeFilterState>(
    fromParams(initialQs, STATIC_TIME_FIELDS, DEFAULT_TIME_FIELD, DEFAULT_DAYS),
  );

  // Drives the stat tiles and the engagement chart ONLY. Kept separate from
  // `timeFilter` deliberately: the summary endpoint takes a plain day count and
  // cannot express a column choice or an absolute bound, so a shared control
  // would have to misreport on every card that could not follow it.
  let sinceDays = $state(30);
  /**
   * The chips, restored from the URL so a filtered list survives a reload and
   * can be shared. `parseFilters` drops anything whose field or operator
   * SESSION_FIELDS does not declare, so a hand-edited link cannot smuggle a
   * chip the catalog would 400 on.
   */
  let filters = $state<Filter[]>(parseFilters(initialQs.getAll('filter'), SESSION_FIELDS));

  /** The text in the box. Editing it queries nothing on its own. */
  let search = $state('');
  /**
   * The query the rows below were actually fetched with — written only by
   * `onSearch` (button, Enter, clear). This page previously fed `search`
   * straight into the load effect with no debounce at all, so every keystroke
   * was a request, and most of those requests carried a half-typed query the
   * reader never meant to run.
   */
  let appliedSearch = $state('');

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
    tf: TimeFilterState,
    sort: string,
    off: number,
    query: string,
    chips: Filter[],
    force = false,
  ) {
    const enc = encodeFilters(chips);
    // The window enters the key as its DECLARATION, never as the instant `last`
    // resolves to — a clock-derived component mints a fresh entry per load, so
    // the cache hits zero times while looking perfectly wired.
    const windowKey = `${tf.field}:${tf.mode}:${tf.lastDays ?? ''}:${tf.from ?? ''}:${tf.to ?? ''}`;
    await sessionsView.load(
      // The chips join the key for the same reason `sort` and `off` do: without
      // them, adding a chip would find the unfiltered page already cached under
      // the same key and repaint it with nothing on the wire, so the filter
      // would look like it silently did nothing.
      viewKey(
        'sessions.list',
        appId,
        sessionStore.scopeKey,
        windowKey,
        sort,
        off,
        LIMIT,
        query,
        enc.join('&'),
      ),
      () => listSessions(appId, {
        sort,
        limit: LIMIT,
        offset: off,
        query: query || undefined,
        filters: enc,
        ...toRecord(tf, DEFAULT_TIME_FIELD),
      }),
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
        load(aid, timeFilter, sortParam(list.sort), list.offset, appliedSearch, filters, true),
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
    const tf = timeFilter;
    const sort = sortParam(list.sort);
    const off = list.offset;
    // `appliedSearch`, never `search`: reading the typed text here is what
    // makes the box fire per keystroke.
    const q = appliedSearch;
    // Read through `$state.snapshot`: `filters` is a deep proxy, and touching
    // it here is what makes a chip change re-run this effect.
    const chips = $state.snapshot(filters) as Filter[];
    if (aid) void load(aid, tf, sort, off, q, chips);
  });

  /**
   * Apply the search box. Also resets to page one — a query change is a
   * predicate change, and row 51 of the old result set is not row 51 of the new.
   */
  function onSearch(q: string) {
    appliedSearch = q;
    list = setOffsetPage(list, 0);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    if (aid) void loadAnalytics(aid, days);
  });

  // Drives the tiles and chart only, so it no longer resets the table's page.
  function onRange(days: number) {
    sinceDays = days;
  }

  function onTimeFilter(v: TimeFilterState) {
    timeFilter = v;
    // A changed predicate invalidates the page position: row 51 of the old
    // window is not row 51 of the new one.
    list = setOffsetPage(list, 0);
  }

  /**
   * A chip was added or removed. Back to page one, for the same reason a new
   * window or a new query goes back: row 51 of the old result set is not row 51
   * of the new one, and an offset kept across a predicate change lands the
   * reader mid-list or past its end.
   *
   * Done here rather than in the URL effect below on purpose — `setOffsetPage`
   * READS `list`, so an effect that reset the page would re-run on its own
   * write and never settle.
   */
  function onFilters() {
    list = setOffsetPage(list, 0);
  }

  // Mirror the window and the chips into the URL so a filtered view survives a
  // refresh and can be shared. `replace`, not `push`: adjusting a filter is not
  // a navigation. `initialQs` is read once at setup rather than through a
  // `$derived`, which is what stops this from feeding itself.
  $effect(() => {
    const params = toParams(timeFilter, DEFAULT_TIME_FIELD);
    for (const f of encodeFilters(filters)) params.append('filter', f);
    const qs = params.toString();
    void replace(qs ? `/sessions?${qs}` : '/sessions');
  });

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
      <h1 class="page-title">{t('explore.column.sessions')}</h1>
      <p class="muted sub">{t('sessions.subtitle')}</p>
    </div>
  </div>

  <div class="analytics-head">
    <h2 class="section-title">{t('sessions.card.engagement')}</h2>
    <DateRange value={sinceDays} onchange={onRange} />
  </div>

  {#if analytics}
    <StatTiles min={160}>
      <StatTile label={t('explore.column.sessions')} value={compactNumber(analytics.stats.sessions)} tone="primary" sub={`last ${sinceDays}d`} />
      <StatTile label={t('sessions.stat.crashed')} value={compactNumber(analytics.stats.crashed)} tone={analytics.stats.crashed > 0 ? 'warning' : 'neutral'} />
      <StatTile label={t('sessions.stat.avg')} value={formatDuration(analytics.stats.avg_session_ms)} />
      <StatTile label={t('sessions.stat.median')} value={formatDuration(analytics.stats.median_session_ms)} />
    </StatTiles>

    <div class="session-charts">
      <Card title={t('sessions.card.avgPerDay')}>
        <TimeSeriesChart data={durationSeries} format={formatDuration} showTotal={false} />
      </Card>
      <Card title={t('sessions.card.distribution')}>
        <DurationHistogram data={analytics.duration_histogram} />
      </Card>
    </div>
  {:else if analyticsError}
    <Card><p class="muted">{analyticsError}</p></Card>
  {/if}

  <!-- Every control that narrows the TABLE sits in this one row, directly above
       it: search, window, refresh, export. They used to live in the page
       header, two sections up, where the search box read as if it filtered the
       engagement charts — which run their own query and ignore it entirely. -->
  <!--
    `showRange={false}`: the page's window is the `<TimeFilter>` in `actions`,
    which also picks the timestamp COLUMN. The bar's own DateRange would be a
    second range picker connected to nothing — a control that reports a window
    the list is not using.

    No hand-written placeholder on the search box either. The old one advertised
    `@tag=v1`, which sessions do not carry: the catalog declares no tag
    dimension for the resource and the resolver refuses `Store::Tag` outright,
    so every query built from that hint came back a 400. The component derives
    its example from the schema it loaded.
  -->
  <div class="list-head">
    <FilterBar
      fields={SESSION_FIELDS}
      bind:filters
      bind:search
      bind:sinceDays
      showRange={false}
      appId={sessionStore.currentAppId ?? undefined}
      context="sessions"
      error={searchError}
      {onSearch}
      onchange={onFilters}
    >
      {#snippet actions()}
        <TimeFilter fields={TIME_FIELDS} value={timeFilter} onchange={onTimeFilter} />
        <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
        <Button
          variant="secondary"
          disabled={sessions.length === 0}
          onclick={downloadSessionsCsv}
          title={t('sessions.exportTitle')}
        >
          <Icon name="download" size={15} />
          {t('explore.exportCsv')}
        </Button>
      {/snippet}
    </FilterBar>
  </div>

  <!-- Above the session rows, not above the engagement charts: it describes
       what the LIST leaves out, and the charts run their own query. -->
  <SearchDisclosure {clamped} />

  <Card padding="none">
    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if error}
      <EmptyState title={t('sessions.error.load')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() =>
              sessionStore.currentAppId &&
              load(
                sessionStore.currentAppId,
                timeFilter,
                sortParam(list.sort),
                list.offset,
                appliedSearch,
                filters,
                true,
              )}
          >
            {t('common.retry')}
          </Button>
        {/snippet}
      </EmptyState>
    {:else if sessions.length === 0}
      <EmptyState
        title={t('sessions.empty.noMatches')}
        description={appliedSearch ? `No sessions match “${appliedSearch}”.` : "No sessions recorded in this range. Widen the date range or send activity from your SDK."}
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
            <th>{t('sessions.column.session')}</th>
            <SortableTh key="distinct_id" columnDefault="asc" sort={list.sort} {onsort}>
              {t('sessions.column.user')}
            </SortableTh>
            <SortableTh key="device_key" columnDefault="asc" sort={list.sort} {onsort}>
              {t('sessions.column.device')}
            </SortableTh>
            <SortableTh key="started_at" sort={list.sort} {onsort}>{t('explore.column.started')}</SortableTh>
            <!-- No stored duration: the server orders by `last_event_at -
                 started_at`, the same interval this column renders. -->
            <SortableTh key="duration_ms" sort={list.sort} {onsort}>{t('explore.column.duration')}</SortableTh>
            <SortableTh key="events_count" class="num" sort={list.sort} {onsort}>
              {t('explore.column.events')}
            </SortableTh>
            <SortableTh key="errors_count" class="num" sort={list.sort} {onsort}>
              {t('explore.column.errors')}
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
              <td class="num">{formatNumber(s.events_count)}</td>
              <td class="num">
                <span class:err={s.errors_count > 0}>{formatNumber(s.errors_count)}</span>
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
        {total}
        totalIsCapped={sessionsView.data?.total_is_capped ?? false}
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
  /* Opens the list section the way `.analytics-head` opens the charts: the gap
     above separates the engagement block from the table, and the FilterBar
     carries its own 16px below. The bar wraps internally, so this only has to
     own the section rhythm. */
  .list-head {
    margin-top: 28px;
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
