<script lang="ts">
  import { t } from '../lib/i18n';
  import { rowHref, rowNav } from '../lib/utils/row-link';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
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
  import { rangeStore } from '../lib/stores/range.svelte';
  import { rangeKey, toParams, type DateRangeValue } from '../lib/models/date-range';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listWorkflows } from '../lib/api/workflows';
  import { countWorkflows } from '../lib/api/counts';
  import { RowCount } from '../lib/stores/row-count.svelte';
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

  // The shared selection, falling back to this page's own 30 days.
  let range = $state<DateRangeValue>(rangeStore.effective(30));
  // `search` is bound to the input; `appliedSearch` is the SUBMITTED value
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
  const rowCount = new RowCount();
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

  function onRange(v: DateRangeValue) {
    range = v;
    rangeStore.set(v);
    list = setOffsetPage(list, 0);
  }

  // Submit-driven, not debounced: only the Search button, Enter and the clear
  // button commit `search` into `appliedSearch`, so typing queries nothing.
  function onSearch(v: string) {
    appliedSearch = v;
    // A changed predicate invalidates the page position: row 51 of the old
    // result set is not row 51 of the new one.
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
    win: DateRangeValue,
    s: string,
    sort: string,
    off: number,
    force = false,
  ) {
    // `rangeKey`, never the resolved instants — a clock-derived key hits zero.
    const rk = rangeKey(win);
    // Predicate only: a total is unchanged by ordering or page boundary.
    void rowCount.load(
      viewKey('workflows.count', appId, sessionStore.scopeKey, rk, s),
      () => countWorkflows(appId, { range: win, search: s || undefined }),
      force,
    );
    await view.load(
      viewKey('workflows.list', appId, sessionStore.scopeKey, rk, s, sort, off, LIMIT),
      () =>
        listWorkflows(appId, {
          ...toParams(win),
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
      await load(aid, range, appliedSearch, sortParam(list.sort), list.offset, true);
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const win = range;
    const s = appliedSearch;
    const sort = sortParam(list.sort);
    const off = list.offset;
    if (aid) void load(aid, win, s, sort, off);
  });
</script>

  <div class="head">
    <div>
      <h1 class="page-title">{t('workflows.title')}</h1>
      <p class="muted sub">{t('workflows.subtitle')}</p>
    </div>
    <div class="controls">
      <DateRange value={range} onchange={onRange} />
      <SearchInput bind:value={search} onsearch={onSearch} placeholder={t('workflows.search')} width="240px" />
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached rows, fetching fresh" hint, and without it
        the instant paint is indistinguishable from live data.
      -->
      <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
    </div>
  </div>

  {#if error && rows.length === 0}
    <Card>
      <EmptyState title={t('workflows.error.load')} description={error} icon="workflow">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, range, appliedSearch, sortParam(list.sort), list.offset, true);
            }}
          >
            {t('common.retry')}
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if loading && rows.length === 0}
    <Skeleton rows={6} />
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title={t('workflows.empty.title')}
        description={appliedSearch
          ? `No workflows match “${appliedSearch}”.`
          : 'Call startWorkflow() in your app to group events into named flows.'}
        icon="workflow"
      />
    </Card>
  {:else}
    <StatTiles min={140}>
      <StatTile label={t('workflows.stat.started')} value={compactNumber(totals.started)} />
      <StatTile label={t('workflows.stat.completed')} value={compactNumber(totals.completed)} tone="success" />
      <StatTile label={t('workflows.stat.completionRate')} value={formatPercent(totalCompletionRate)} />
      <StatTile
        label={t('workflows.stat.abandoned')}
        value={compactNumber(totals.abandoned)}
        tone={totals.abandoned > 0 ? 'error' : 'neutral'}
      />
    </StatTiles>

    <Card padding="none">
      <DataTable>
        {#snippet head()}
          <tr>
            <SortableTh key="name" columnDefault="asc" sort={list.sort} {onsort}>
              {t('workflows.column.workflow')}
            </SortableTh>
            <SortableTh key="started" class="num" sort={list.sort} {onsort}>{t('workflows.stat.started')}</SortableTh>
            <SortableTh key="completed" class="num" sort={list.sort} {onsort}>
              {t('workflows.stat.completed')}
            </SortableTh>
            <SortableTh key="cancelled" class="num" sort={list.sort} {onsort}>
              {t('workflows.stat.cancelled')}
            </SortableTh>
            <SortableTh key="abandoned" class="num" sort={list.sort} {onsort}>
              {t('workflows.stat.abandoned')}
            </SortableTh>
            <!-- `completion_rate` has no column: the server orders by the same
                 `completed / started` ratio this cell computes client-side. -->
            <SortableTh key="completion_rate" class="num" sort={list.sort} {onsort}>
              {t('workflows.stat.completionRate')}
            </SortableTh>
            <SortableTh key="median_duration_ms" class="num" sort={list.sort} {onsort}>
              {t('workflows.column.median')}
            </SortableTh>
            <SortableTh key="p95_duration_ms" class="num" sort={list.sort} {onsort}>
              p95
            </SortableTh>
            <!-- `users` ON THE WIRE, even though the row field and the SQL
                 alias are both `unique_users`. Sending `unique_users` is a
                 400, not a silently ignored parameter. -->
            <SortableTh key="users" class="num" sort={list.sort} {onsort}>{t('users.title')}</SortableTh>
            <SortableTh key="last_seen" class="num" sort={list.sort} {onsort}>
              {t('explore.column.lastSeen')}
            </SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as r (r.name)}
            {@const path = '/workflows/' + encodeURIComponent(r.name)}
            <tr
              class="clickable"
              onclick={(e) => rowNav(e, path)}
              onauxclick={(e) => rowNav(e, path)}
            >
              <td>
                <a class="row-link cell-mono truncate" href={rowHref(path)}>{r.name}</a>
              </td>
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
        total={rowCount.total}
        totalIsCapped={rowCount.isCapped}
        onchange={(o) => (list = setOffsetPage(list, o))}
      />
    </Card>
  {/if}

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
  .num {
    text-align: end;
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
