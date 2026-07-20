<script lang="ts">
  import { querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import {
    EVENT_FIELDS,
    encodeFilters,
    parseFilters,
    type Filter,
    type FieldDef,
  } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { topEvents, eventSeries, listEvents } from '../lib/api/events';
  import { listEnvironments } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { relativeTime, formatDateTime } from '../lib/utils/format';
  import type { AnalyticsEvent, SeriesPoint, TopEvent } from '../lib/models';

  const STREAM_LIMIT = 50;

  // Hydrate filter/search/date-range state from the URL once, at init — not
  // inside an effect, so this never re-runs and never fights the sync effect
  // below.
  const initial = new URLSearchParams($querystring ?? '');
  let filters = $state<Filter[]>(parseFilters(initial.getAll('filter'), EVENT_FIELDS));
  let search = $state(initial.get('q') ?? '');
  // The URL-sync/reload and stream-load effects below depend on this, not on
  // `search` directly, so free-text typing doesn't fire a backend request +
  // history.replaceState on every keystroke. Filters and the date range still
  // apply immediately.
  let appliedSearch = $state(initial.get('q') ?? '');
  let sinceDays = $state(Number(initial.get('since_days')) || 30);

  // `environment` field options are loaded per-app; start from the static
  // defs and inject a fresh copy once environments resolve (never mutate the
  // exported EVENT_FIELDS array in place).
  let eventFields = $state<FieldDef[]>(EVENT_FIELDS);

  const selectedTopEvent = $derived(
    filters.find((f) => f.field === 'name' && f.op === 'eq')?.value ?? null,
  );

  let top = $state<TopEvent[]>([]);
  let series = $state<SeriesPoint[]>([]);
  let loadingTop = $state(true);
  let loadingSeries = $state(true);
  let error = $state<string | null>(null);
  let refreshing = $state(false);

  // Raw event stream state.
  let streamOffset = $state(0);
  let streamEvents = $state<AnalyticsEvent[]>([]);
  let loadingStream = $state(true);
  let streamError = $state<string | null>(null);
  let expandedId = $state<string | null>(null);

  async function loadEnvironmentOptions(appId: string) {
    let names: string[] = [];
    try {
      const envs = await listEnvironments(appId);
      names = envs.map((e) => e.name);
    } catch {
      names = [];
    }
    eventFields = EVENT_FIELDS.map((f) =>
      f.key === 'environment' ? { ...f, options: names } : f,
    );
  }

  async function loadTop(appId: string, days: number) {
    loadingTop = true;
    error = null;
    try {
      top = await topEvents(appId, { since_days: days, limit: 12 });
    } catch (err) {
      error = errorMessage(err);
      top = [];
    } finally {
      loadingTop = false;
    }
  }

  async function loadSeries(appId: string, days: number, name: string | null) {
    loadingSeries = true;
    try {
      series = await eventSeries(appId, {
        since_days: days,
        name: name ?? undefined,
      });
    } catch (err) {
      error = errorMessage(err);
      series = [];
    } finally {
      loadingSeries = false;
    }
  }

  async function loadStream(
    appId: string,
    filterList: string[],
    q: string,
    days: number,
    offset: number,
  ) {
    loadingStream = true;
    streamError = null;
    try {
      streamEvents = await listEvents(appId, {
        filters: filterList,
        q: q.trim() || undefined,
        sinceDays: days,
        limit: STREAM_LIMIT,
        offset,
      });
    } catch (err) {
      streamError = errorMessage(err);
      streamEvents = [];
    } finally {
      loadingStream = false;
    }
  }

  // Re-fetch all page data with the current state (filters, search, date
  // range and pagination) left intact. Reuses the existing loaders.
  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([
        loadTop(aid, sinceDays),
        loadSeries(aid, sinceDays, selectedTopEvent),
        loadStream(aid, encodeFilters(filters), appliedSearch, sinceDays, streamOffset),
      ]);
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    if (aid) void loadEnvironmentOptions(aid);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void loadTop(aid, days);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    const name = selectedTopEvent;
    if (aid) void loadSeries(aid, days, name);
  });

  // Debounce free-text search only: filters + date range should reload
  // immediately, but typing in the search box should settle before we requery.
  let searchTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const s = search;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      appliedSearch = s;
    }, 300);
    return () => clearTimeout(searchTimer);
  });

  // Re-query + rewrite the URL whenever filter/appliedSearch/date-range state
  // changes, and reset the stream back to page one. Depends on
  // `appliedSearch` (debounced), not `search`, so this doesn't fire per
  // keystroke.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    const enc = encodeFilters(filters);
    const s = appliedSearch;
    const days = sinceDays;
    if (!aid) return;
    const p = new URLSearchParams();
    for (const f of enc) p.append('filter', f);
    if (s) p.set('q', s);
    p.set('since_days', String(days));
    void replace(`/events?${p.toString()}`);
    streamOffset = 0;
    expandedId = null;
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const enc = encodeFilters(filters);
    const s = appliedSearch;
    const days = sinceDays;
    const offset = streamOffset;
    if (aid) void loadStream(aid, enc, s, days, offset);
  });

  function selectTopEvent(name: string) {
    const rest = filters.filter((f) => !(f.field === 'name' && f.op === 'eq'));
    filters = [...rest, { field: 'name', op: 'eq', value: name }];
  }

  function toggleRow(id: string) {
    expandedId = expandedId === id ? null : id;
  }

  function propsPreview(props: Record<string, unknown> | null): string {
    if (!props) return '';
    const entries = Object.entries(props);
    if (entries.length === 0) return '';
    return entries.map(([k, v]) => `${k}: ${scalar(v)}`).join('  ·  ');
  }

  function scalar(v: unknown): string {
    if (v === null) return 'null';
    if (Array.isArray(v)) return `[${v.length}]`;
    if (typeof v === 'object') return '{…}';
    const s = String(v);
    return s.length > 48 ? `${s.slice(0, 48)}…` : s;
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Events</h1>
      <p class="muted sub">Product analytics — event volume, top events and raw stream.</p>
    </div>
    <div class="controls">
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  <p class="hint muted">Filter by <code>Tag</code> (key = value); the search box also matches tag &amp; payload content.</p>
  <FilterBar fields={eventFields} bind:filters bind:search bind:sinceDays />

  {#if error && top.length === 0 && series.length === 0}
    <Card>
      <EmptyState title="Couldn't load analytics" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) {
                loadTop(aid, sinceDays);
                loadSeries(aid, sinceDays, selectedTopEvent);
              }
            }}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <div class="grid">
      <Card title="Event volume">
        {#if loadingSeries}
          <div class="center"><Spinner size={22} /></div>
        {:else}
          <TimeSeriesChart data={series} height={220} color="var(--primary)" />
        {/if}
      </Card>

      <Card title="Top events">
        {#if loadingTop}
          <div class="center"><Spinner size={22} /></div>
        {:else if top.length === 0}
          <EmptyState title="No events yet" description="Send events from your SDK to see them here." icon="chart-column" />
        {:else}
          <p class="hint muted">Click an event to filter the chart and stream.</p>
          <BarList items={top} selected={selectedTopEvent} onselect={selectTopEvent} />
        {/if}
      </Card>
    </div>

    <Card padding="none" title="Event stream">
      {#if loadingStream && streamEvents.length === 0}
        <div class="center"><Spinner size={22} /></div>
      {:else if streamError && streamEvents.length === 0}
        <EmptyState title="Couldn't load events" description={streamError} icon="triangle-alert">
          {#snippet action()}
            <Button
              variant="secondary"
              onclick={() => {
                const aid = sessionStore.currentAppId;
                if (aid) loadStream(aid, encodeFilters(filters), appliedSearch, sinceDays, streamOffset);
              }}
            >
              Retry
            </Button>
          {/snippet}
        </EmptyState>
      {:else if streamEvents.length === 0}
        <EmptyState
          title="No events"
          description={search || filters.length > 0
            ? 'No events match the current filters on this page.'
            : 'No raw events in this range yet.'}
          icon="search"
        />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <th class="col-name">Event</th>
              <th>User</th>
              <th>Session</th>
              <th class="col-props">Properties</th>
              <th class="col-time">Time</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each streamEvents as ev (ev.id)}
              <tr class="clickable" onclick={() => toggleRow(ev.id)}>
                <td>
                  <span class="ev-caret" class:open={expandedId === ev.id}><Icon name="chevron-right" size={13} /></span>
                  <span class="ev-name">{ev.name}</span>
                </td>
                <td>
                  {#if ev.distinct_id}
                    <a
                      class="link mono trunc"
                      href={`#/persons/${encodeURIComponent(ev.distinct_id)}`}
                      onclick={(e) => e.stopPropagation()}
                      title={ev.distinct_id}
                    >
                      {ev.distinct_id}
                    </a>
                  {:else}
                    <span class="muted">anonymous</span>
                  {/if}
                </td>
                <td>
                  {#if ev.session_id}
                    <a
                      class="link mono trunc"
                      href={`#/sessions/${encodeURIComponent(ev.session_id)}`}
                      onclick={(e) => e.stopPropagation()}
                      title={ev.session_id}
                    >
                      {ev.session_id}
                    </a>
                  {:else}
                    <span class="faint">—</span>
                  {/if}
                </td>
                <td class="col-props">
                  {#if propsPreview(ev.properties)}
                    <span class="props-prev mono">{propsPreview(ev.properties)}</span>
                  {:else}
                    <span class="faint">—</span>
                  {/if}
                </td>
                <td class="muted" title={formatDateTime(ev.occurred_at)}>
                  {relativeTime(ev.occurred_at)}
                </td>
              </tr>
              {#if expandedId === ev.id}
                <tr class="detail-row">
                  <td colspan={5}>
                    {#if ev.screen}
                      <a
                        class="screen-link mono"
                        href={`#/screens/${encodeURIComponent(ev.screen)}`}
                        onclick={(e) => e.stopPropagation()}
                      >
                        <Icon name="layout-panel-top" size={13} />{ev.screen}
                      </a>
                    {/if}
                    {#if ev.properties && Object.keys(ev.properties).length > 0}
                      <JsonTree value={ev.properties} name="properties" expandTo={2} />
                    {:else}
                      <span class="faint">No properties on this event.</span>
                    {/if}
                    {#if ev.tags && Object.keys(ev.tags).length > 0}
                      <JsonTree value={ev.tags} name="tags" expandTo={2} />
                    {/if}
                    {#if ev.contexts && Object.keys(ev.contexts).length > 0}
                      <JsonTree value={ev.contexts} name="contexts" expandTo={2} />
                    {/if}
                    {#if ev.extra && Object.keys(ev.extra).length > 0}
                      <JsonTree value={ev.extra} name="extra" expandTo={2} />
                    {/if}
                  </td>
                </tr>
              {/if}
            {/each}
          {/snippet}
        </DataTable>
        <div class="pager-wrap">
          <Pagination
            offset={streamOffset}
            limit={STREAM_LIMIT}
            count={streamEvents.length}
            onchange={(o) => {
              streamOffset = o;
              expandedId = null;
            }}
          />
        </div>
      {/if}
    </Card>
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
  .grid {
    display: grid;
    grid-template-columns: 1.6fr 1fr;
    gap: 18px;
    margin-bottom: 18px;
    align-items: start;
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 200px;
  }
  .hint {
    font-size: 12px;
    margin-bottom: 12px;
  }

  /* Event stream table */
  .col-name {
    min-width: 160px;
  }
  .col-props {
    width: 100%;
    max-width: 0;
  }
  .col-time {
    white-space: nowrap;
  }
  .ev-caret {
    display: inline-block;
    font-size: 8px;
    color: var(--text-faint);
    transition: transform 0.12s ease;
    margin-right: 7px;
  }
  .ev-caret.open {
    transform: rotate(90deg);
  }
  .ev-name {
    font-weight: 560;
    color: var(--text);
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
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 12px;
  }
  .props-prev {
    display: inline-block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 11.5px;
    color: var(--text-muted);
  }
  .detail-row :global(td) {
    background: var(--surface-2);
    padding: 12px 16px 14px 32px;
  }
  .screen-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-bottom: 10px;
    font-size: 12px;
    font-weight: 550;
    color: var(--primary);
  }
  .screen-link:hover {
    text-decoration: underline;
  }
  .pager-wrap {
    padding: 10px 14px 12px;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
