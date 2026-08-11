<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listWorkflows } from '../lib/api/workflows';
  import {
    setOffsetPage,
    setOffsetSort,
    type ListPage,
    type OffsetListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import { completionRate, formatDuration } from '../lib/workflows';
  import { compactNumber, formatPercent } from '../lib/utils/format';
  import type { WorkflowRow } from '../lib/models';

  const LIMIT = 50;

  let sinceDays = $state(30);
  // `search` is bound to the input; `appliedSearch` is the debounced value
  // that drives loads (same split as Issues.svelte).
  let search = $state('');
  let appliedSearch = $state('');

  // `started` descending is the endpoint's own default, so this describes the
  // first request rather than changing it.
  let list = $state<OffsetListState>({ sort: { key: 'started', dir: 'desc' }, offset: 0 });

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  // Cached view (lib/stores/cached-view.svelte.ts): cached rows paint instantly on
  // return and refresh behind a spinner. Re-exposed under the names the template
  // already used, so the markup is unchanged apart from the refresh control —
  // `loading` still means "nothing to show", and `revalidating` is the new
  // "rows are up, fetching fresh behind them".
  const view = new CachedView<ListPage<WorkflowRow>>();

  const rows = $derived(view.data?.rows ?? []);
  // Read off the cached payload, not a separate `$state` set on the network
  // path: a cache HIT repaints rows without fetching, and a `hasNext` only the
  // fetch updates would be the previous key's answer.
  const hasNext = $derived(view.data?.hasNext ?? false);
  const loading = $derived(view.loading);
  const revalidating = $derived(view.revalidating);
  const error = $derived(view.error);

  let refreshing = $state(false);

  const totals = $derived(
    rows.reduce(
      (acc, r) => ({
        started: acc.started + r.started,
        completed: acc.completed + r.completed,
        abandoned: acc.abandoned + r.abandoned,
      }),
      { started: 0, completed: 0, abandoned: 0 },
    ),
  );
  const totalCompletionRate = $derived(
    totals.started === 0 ? 0 : totals.completed / totals.started,
  );

  function onRange(days: number) {
    sinceDays = days;
    list = setOffsetPage(list, 0);
  }

  function onSearch() {
    list = setOffsetPage(list, 0);
  }

  /**
   * `force` bypasses the fresh-window short-circuit: the Refresh button and the
   * error-state Retry both mean "go to the network now".
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's workflows would be served as
   * another's.
   *
   * `sort` is in it for a related reason: without it a header click finds the
   * previous ordering already cached under the same key and repaints it with
   * NO request on the wire, so the sort looks like it silently did nothing.
   */
  async function load(
    appId: string,
    days: number,
    s: string,
    sort: string,
    off: number,
    force = false,
  ) {
    await view.load(
      viewKey('workflows.list', appId, sessionStore.scopeKey, days, s, sort, off, LIMIT),
      () =>
        listWorkflows(appId, {
          since_days: days,
          search: s || undefined,
          sort,
          limit: LIMIT,
          offset: off,
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
      await load(aid, sinceDays, appliedSearch, sortParam(list.sort), list.offset, true);
    } finally {
      refreshing = false;
    }
  }

  // Debounce free-text search only: typing in the search box should settle
  // before we requery; the date range applies immediately.
  let searchTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const s = search;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      appliedSearch = s;
      list = setOffsetPage(list, 0);
    }, 300);
    return () => clearTimeout(searchTimer);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    const s = appliedSearch;
    const sort = sortParam(list.sort);
    const off = list.offset;
    if (aid) void load(aid, days, s, sort, off);
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Workflows</h1>
      <p class="muted sub">Named, bounded spans of activity your app reports.</p>
    </div>
    <div class="controls">
      <DateRange value={sinceDays} onchange={onRange} />
      <SearchInput bind:value={search} oninput={onSearch} placeholder="Search workflows…" width="240px" />
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached rows, fetching fresh" hint, and without it
        the instant paint is indistinguishable from live data.
      -->
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
    </div>
  </div>

  {#if error && rows.length === 0}
    <Card>
      <EmptyState title="Couldn't load workflows" description={error} icon="workflow">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, sinceDays, appliedSearch, sortParam(list.sort), list.offset, true);
            }}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if loading && rows.length === 0}
    <div class="center"><Spinner size={26} /></div>
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title="No workflows yet"
        description={appliedSearch
          ? `No workflows match “${appliedSearch}”.`
          : 'Call startWorkflow() in your app to group events into named flows.'}
        icon="workflow"
      />
    </Card>
  {:else}
    <StatTiles min={140}>
      <StatTile label="Started" value={compactNumber(totals.started)} />
      <StatTile label="Completed" value={compactNumber(totals.completed)} tone="success" />
      <StatTile label="Completion rate" value={formatPercent(totalCompletionRate)} />
      <StatTile
        label="Abandoned"
        value={compactNumber(totals.abandoned)}
        tone={totals.abandoned > 0 ? 'error' : 'neutral'}
      />
    </StatTiles>

    <Card padding="none">
      <DataTable>
        {#snippet head()}
          <tr>
            <SortableTh key="name" columnDefault="asc" sort={list.sort} {onsort}>
              Workflow
            </SortableTh>
            <SortableTh key="started" class="num" sort={list.sort} {onsort}>Started</SortableTh>
            <SortableTh key="completed" class="num" sort={list.sort} {onsort}>
              Completed
            </SortableTh>
            <SortableTh key="cancelled" class="num" sort={list.sort} {onsort}>
              Cancelled
            </SortableTh>
            <SortableTh key="abandoned" class="num" sort={list.sort} {onsort}>
              Abandoned
            </SortableTh>
            <!-- `completion_rate` has no column: the server orders by the same
                 `completed / started` ratio this cell computes client-side. -->
            <SortableTh key="completion_rate" class="num" sort={list.sort} {onsort}>
              Completion rate
            </SortableTh>
            <SortableTh key="median_duration_ms" class="num" sort={list.sort} {onsort}>
              Median
            </SortableTh>
            <SortableTh key="p95_duration_ms" class="num" sort={list.sort} {onsort}>
              p95
            </SortableTh>
            <!-- `users` ON THE WIRE, even though the row field and the SQL
                 alias are both `unique_users`. Sending `unique_users` is a
                 400, not a silently ignored parameter. -->
            <SortableTh key="users" class="num" sort={list.sort} {onsort}>Users</SortableTh>
            <SortableTh key="last_seen" class="num" sort={list.sort} {onsort}>
              Last seen
            </SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as r (r.name)}
            <tr class="clickable" onclick={() => push('/workflows/' + encodeURIComponent(r.name))}>
              <td><span class="cell-mono truncate">{r.name}</span></td>
              <td class="num">{compactNumber(r.started)}</td>
              <td class="num">{compactNumber(r.completed)}</td>
              <td class="num">{compactNumber(r.cancelled)}</td>
              <td class="num"><span class:err={r.abandoned > 0}>{compactNumber(r.abandoned)}</span></td>
              <td class="num">{formatPercent(completionRate(r))}</td>
              <td class="num">{formatDuration(r.median_duration_ms)}</td>
              <td class="num">{formatDuration(r.p95_duration_ms)}</td>
              <td class="num">{compactNumber(r.unique_users)}</td>
              <td class="num"><TimeValue value={r.last_seen} /></td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>

      <!-- `hasNext` is the client's `limit + 1` over-fetch probe, not an
           inference from the row count: a final page of exactly `LIMIT` rows
           used to offer a Next that led to an empty page. -->
      <Pagination
        offset={list.offset}
        limit={LIMIT}
        count={rows.length}
        {hasNext}
        onchange={(o) => (list = setOffsetPage(list, o))}
      />
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
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .num {
    text-align: right;
  }
  .truncate {
    display: inline-block;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
</style>
