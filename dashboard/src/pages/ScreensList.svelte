<script lang="ts">
  import { t } from '../lib/i18n';
  import { rowHref, rowNav } from '../lib/utils/row-link';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import { rangeKey, type DateRangeValue } from '../lib/models/date-range';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
  import { refreshRollups } from '../lib/api/rollups';
  import { approx } from '../lib/models/freshness';
  import { rollupState } from '../lib/stores/rollups.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listScreens } from '../lib/api/screens';
  import { countScreens } from '../lib/api/counts';
  import { RowCount } from '../lib/stores/row-count.svelte';
  import {
    setOffsetPage,
    setOffsetSort,
    type ListPage,
    type OffsetListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import { compactNumber, formatDuration } from '../lib/utils/format';
  import type { ScreenRow } from '../lib/models';

  const LIMIT = 50;

  // The shared selection, falling back to this page's own 30 days.
  let range = $state<DateRangeValue>(rangeStore.effective(30));
  // `query` is bound to the input; `search` is the SUBMITTED value that drives loads.
  let query = $state('');
  let search = $state('');

  // `views` descending is the endpoint's own default, so this describes the
  // first request rather than changing it.
  let list = $state<OffsetListState>({ sort: { key: 'views', dir: 'desc' }, offset: 0 });

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  // Cached view (lib/stores/cached-view.svelte.ts): rows already fetched paint
  // instantly on return, then refresh behind a spinner instead of a skeleton.
  // Re-exposed under the names the template already used, so the markup is
  // unchanged apart from the refresh indicator.
  const view = new CachedView<ListPage<ScreenRow>>();
  const rows = $derived(view.data?.rows ?? []);
  // Read off the cached payload, not a separate `$state` set on the network
  // path: a cache HIT repaints rows without fetching, and a `hasNext` only the
  // fetch updates would be the previous key's answer.
  const hasNext = $derived(view.data?.hasNext ?? false);
  const rowCount = new RowCount();
  const revalidating = $derived(view.revalidating);
  const loading = $derived(view.loading);
  let refreshing = $state(false);
  const error = $derived(view.error);

  // Submit-driven, not debounced: `query` is the text in the box and
  // `search` is what the request carries. Only the Search button, Enter and
  // the clear button move one into the other, so typing never queries.
  function onSearch(v: string) {
    search = v.trim();
    // A changed predicate invalidates the page position: row 51 of the old
    // result set is not row 51 of the new one.
    list = setOffsetPage(list, 0);
  }

  function onRange(v: DateRangeValue) {
    range = v;
    rangeStore.set(v);
    list = setOffsetPage(list, 0);
  }

  // `scopeKey` must be in the key: it carries the selected environment, which the
  // axios interceptor adds to the request but which appears in none of these
  // arguments. Omit it and one environment's rows are served as another's.
  //
  // `sort` must be in it for a related reason: without it a header click finds
  // the previous ordering already cached under the same key and repaints it
  // with NO request on the wire, so the sort looks like it silently did
  // nothing.
  //
  // `force` bypasses the fresh-window short-circuit — an explicit Refresh or
  // Retry means "go to the network now".
  async function load(
    appId: string,
    win: DateRangeValue,
    s: string,
    sort: string,
    off: number,
    force = false,
  ) {
    // `rangeKey`, never the resolved instants: a key derived from the clock
    // mints a fresh entry per load and hits zero times.
    const rk = rangeKey(win);
    // Keyed on the PREDICATE only — no `sort`, no `off`, no `LIMIT`. A total
    // does not change when you reorder or page, so folding either in would
    // refetch the count on every click for an answer that cannot differ.
    void rowCount.load(
      viewKey('screens.count', appId, sessionStore.scopeKey, rk, s),
      () => countScreens(appId, { range: win, search: s || undefined }),
      force,
    );
    await view.load(
      viewKey('screens.list', appId, sessionStore.scopeKey, rk, s, sort, off, LIMIT),
      () => listScreens(appId, {
        q: s || undefined,
        window: win,
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
      // Kick an immediate rollup fold first (bounded server-side wait), so
      // the reloads below fetch aggregates that include the newest events.
      // Older APIs 404 this — then the reload alone is the refresh.
      await refreshRollups(aid).catch(() => {});
      await Promise.all([
        load(aid, range, search, sortParam(list.sort), list.offset, true),
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
    const win = range;
    const s = search;
    const sort = sortParam(list.sort);
    const off = list.offset;
    if (aid) void load(aid, win, s, sort, off);
  });
</script>

  <div class="head">
    <div>
      <h1 class="page-title">{t('screens.title')}</h1>
      <p class="muted sub">{t('screens.subtitle')}</p>
    </div>
    <div class="controls">
      <DateRange value={range} onchange={onRange} />
      <SearchInput bind:value={query} onsearch={onSearch} placeholder={t('screens.search')} width="240px" />
      <RollupChip />
      <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
    </div>
  </div>

  {#if error && rows.length === 0}
    <Card>
      <EmptyState title={t('screens.error.load')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, range, search, sortParam(list.sort), list.offset);
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
        title={t('screens.empty.title')}
        description={search
          ? `No screens match “${search}”.`
          : 'Call setScreen() in your SDK to attribute events to screens.'}
        icon="layout-panel-top"
      />
    </Card>
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <SortableTh key="screen" columnDefault="asc" sort={list.sort} {onsort}>{t('screens.column.screen')}</SortableTh>
          <SortableTh key="views" class="num" sort={list.sort} {onsort}>{t('screens.column.views')}</SortableTh>
          <SortableTh key="events" class="num" sort={list.sort} {onsort}>{t('explore.column.events')}</SortableTh>
          <SortableTh key="exceptions" class="num" sort={list.sort} {onsort}>
            {t('screens.column.exceptions')}
          </SortableTh>
          <SortableTh key="users" class="num" sort={list.sort} {onsort}>{t('users.title')}</SortableTh>
          <SortableTh key="avg_dwell_ms" class="num" sort={list.sort} {onsort}>
            {t('screens.column.avgDwell')}
          </SortableTh>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each rows as r (r.screen)}
          {@const path = '/screens/' + encodeURIComponent(r.screen)}
          <tr
            class="clickable"
            onclick={(e) => rowNav(e, path)}
            onauxclick={(e) => rowNav(e, path)}
          >
            <td>
              <a class="row-link cell-mono truncate" href={rowHref(path)}>{r.screen}</a>
            </td>
            <td class="num">{compactNumber(r.views)}</td>
            <td class="num">{compactNumber(r.events)}</td>
            <td class="num"><span class:err={r.exceptions > 0}>{compactNumber(r.exceptions)}</span></td>
            <td class="num">{approx(compactNumber(r.users), rollupState.ready)}</td>
            <td class="num">{formatDuration(r.avg_dwell_ms)}</td>
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
