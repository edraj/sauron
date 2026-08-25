<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import SankeyChart from '../lib/components/SankeyChart.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
  import { refreshRollups } from '../lib/api/rollups';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import { rangeKey, toParams, type DateRangeValue } from '../lib/models/date-range';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { getJourney } from '../lib/api/journeys';
  import {
    JOURNEY_TRANSITION_DEFAULT_SORT,
    journeyTransitionAccessor,
  } from '../lib/models/journey-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import type { Journey } from '../lib/models';

  const DEPTHS = [2, 3, 4, 5, 6, 7, 8];

  // Seeded from the SHARED selection, falling back to this page's own 30
  // days until the user has chosen one — see `stores/range.svelte.ts` for
  // why the store starts empty rather than defaulting.
  let range = $state<DateRangeValue>(rangeStore.effective(30));
  let depth = $state(5);

  // Cached view (lib/stores/cached-view.svelte.ts): the cached graph paints
  // instantly on return, then refreshes behind the "Updating…" indicator.
  // Re-exposed under the template's existing names, so the markup is unchanged
  // apart from the two spots that now distinguish "nothing yet" (`loading`)
  // from "refreshing over data" (`revalidating`).
  const journeyView = new CachedView<Journey>();

  const journey = $derived(journeyView.data ?? null);
  const loading = $derived(journeyView.loading);
  const revalidating = $derived(journeyView.revalidating);
  const error = $derived(journeyView.error);

  let refreshing = $state(false);

  /**
   * `force` bypasses the fresh-window short-circuit: the Refresh button and the
   * error-state Retry both mean "go to the network now".
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's journey would be served as another's.
   */
  async function load(appId: string, win: DateRangeValue, d: number, force = false) {
    await journeyView.load(
      // `rangeKey`, never the resolved instants: a key derived from the clock
      // mints a fresh entry per load and hits zero times.
      viewKey('journeys.graph', appId, sessionStore.scopeKey, rangeKey(win), d),
      () => getJourney(appId, { ...toParams(win), depth: d }),
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const win = range;
    const d = depth;
    if (aid) void load(aid, win, d);
  });

  const entryPoints = $derived(
    journey
      ? journey.nodes
          .filter((n) => n.step === 0)
          .sort((a, b) => b.count - a.count)
          .map((n) => ({ name: n.event, count: n.count }))
      : [],
  );

  // WHICH ten rows the table shows — the ten highest-count links. Kept as its
  // own step because it is the table's definition, not its order: the card is
  // titled "Top transitions", and sorting the full link list by From and then
  // taking ten would fill that card with the ten alphabetically-first
  // transitions, most of which are top nothing. `[...journey.links]` copies
  // before sorting, so the cached graph the Sankey renders from is never
  // reordered underneath it.
  const topTransitions = $derived(
    journey ? [...journey.links].sort((a, b) => b.count - a.count).slice(0, 10) : [],
  );

  // The display order of those ten, seeded to reproduce the count-descending
  // order they are selected in — so the table opens looking exactly as it did
  // and the caret names the column that produced it. The sort runs over the
  // whole ten-row array, which IS the whole table; there is no pager, because
  // ten rows have no page two.
  //
  // A bare `SortState`, not the `OffsetListState` the paginated tables use:
  // that type exists to make "apply a sort" and "reset to page 1" one
  // indivisible step, and with no offset there is nothing to reset. Its
  // `key`/`dir` are `readonly` (see `sort.ts`), so `sort.dir = 'asc'` is a
  // type error and every transition goes through `toggleSort`.
  let transitionSort = $state<SortState>(JOURNEY_TRANSITION_DEFAULT_SORT);

  const sortedTransitions = $derived(
    sortRows(
      topTransitions,
      journeyTransitionAccessor(transitionSort.key),
      transitionSort.dir,
    ),
  );

  function onTransitionSort(key: string, columnDefault: SortDir) {
    transitionSort = toggleSort(transitionSort, key, columnDefault);
  }

  function retry() {
    const aid = sessionStore.currentAppId;
    // force: a Retry that honoured the cache would re-show the same failure.
    if (aid) void load(aid, range, depth, true);
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
      // force: an explicit click must reach the network regardless of freshness.
      await load(aid, range, depth, true);
    } finally {
      refreshing = false;
    }
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">{t('journeys.title')}</h1>
      <p class="muted sub">{t('journeys.subtitle')}</p>
    </div>
    <div class="controls">
      <div class="control">
        <span class="ctrl-label">{t('journeys.range')}</span>
        <DateRange
          value={range}
          onchange={(v) => {
            range = v;
            rangeStore.set(v);
          }}
        />
      </div>
      <div class="control">
        <span class="ctrl-label">{t('journeys.depth')}</span>
        <div class="depths" role="tablist" aria-label={t('journeys.depthLabel')}>
          {#each DEPTHS as d (d)}
            <button
              class="depth"
              class:active={depth === d}
              onclick={() => (depth = d)}
              type="button"
              role="tab"
              aria-selected={depth === d}
            >
              {d}
            </button>
          {/each}
        </div>
      </div>
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached data, fetching fresh" hint, and without it
        the instant paint is indistinguishable from live data.
      -->
      <RollupChip />
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
    </div>
  </div>

  {#if error && !journey}
    <Card>
      <EmptyState title={t('journeys.error.load')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>{t('common.retry')}</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if loading && !journey}
    <Card>
      <div class="center"><Spinner size={24} /></div>
    </Card>
  {:else if journey && journey.nodes.length === 0}
    <Card>
      <EmptyState
        title={t('journeys.empty.title')}
        description={t('journeys.empty.body')}
        icon="compass"
      />
    </Card>
  {:else if journey}
    <div class="journey-card">
      <!--
        `revalidating`, not `loading`. This sits inside the `journey` branch, and
        `loading` now means "nothing to show at all" — which can never be true
        while `journey` is non-null, so keeping `loading` here would leave the
        indicator permanently dead. `revalidating` is exactly the case it was
        written for: a graph on screen with a refresh in flight behind it.
      -->
      {#if revalidating}
        <div class="reloading"><Spinner size={16} /><span class="faint">{t('funnels.updating')}</span></div>
      {/if}
      <Card title={t('journeys.card.userJourneys')}>
        <SankeyChart {journey} height={480} />
        <p class="caption muted">
          {t('journeys.explainer')}
        </p>
      </Card>
    </div>

    <div class="grid">
      <Card title={t('journeys.card.entryPoints')}>
        {#if entryPoints.length === 0}
          <p class="faint empty-inline">{t('journeys.empty.entries')}</p>
        {:else}
          <p class="hint muted">{t('journeys.entryExplainer')}</p>
          <BarList items={entryPoints} valueLabel="users" />
        {/if}
      </Card>

      <Card title={t('journeys.card.transitions')} padding="none">
        {#if sortedTransitions.length === 0}
          <p class="faint empty-inline pad">{t('journeys.empty.transitions')}</p>
        {:else}
          <DataTable>
            {#snippet head()}
              <tr>
                <SortableTh key="from" columnDefault="asc" sort={transitionSort} onsort={onTransitionSort}>
                  {t('journeys.from')}
                </SortableTh>
                <!-- The arrow glyph. Nothing to order by. -->
                <th></th>
                <SortableTh key="to" columnDefault="asc" sort={transitionSort} onsort={onTransitionSort}>
                  To
                </SortableTh>
                <SortableTh key="users" class="num" sort={transitionSort} onsort={onTransitionSort}>
                  {t('users.title')}
                </SortableTh>
              </tr>
            {/snippet}
            {#snippet children()}
              <!-- Keyed by the transition itself, not by its index: the rows
                   reorder now, and an index key makes Svelte rewrite every
                   cell in place instead of moving the row that moved. The
                   triple is unique — the backend groups links by exactly
                   (from_step, from_event, to_event). -->
              {#each sortedTransitions as tr (tr.from_step + ':' + tr.from_event + '>' + tr.to_event)}
                <tr>
                  <td>
                    <span class="mono">{tr.from_event}</span>
                    <span class="faint step-tag">step {tr.from_step + 1}</span>
                  </td>
                  <td class="arrow faint">→</td>
                  <td><span class="mono">{tr.to_event}</span></td>
                  <td class="num">{formatNumber(tr.count)}</td>
                </tr>
              {/each}
            {/snippet}
          </DataTable>
        {/if}
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
    align-items: flex-end;
    gap: 16px;
    flex-wrap: wrap;
  }
  .control {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .ctrl-label {
    font-size: 10.5px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .depths {
    display: inline-flex;
    gap: 4px;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .depth {
    min-width: 30px;
    padding: 6px 9px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    border-radius: var(--radius-sm);
    font-variant-numeric: tabular-nums;
  }
  .depth:hover {
    color: var(--text);
  }
  .depth.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 260px;
  }
  .journey-card {
    position: relative;
    margin-bottom: 18px;
  }
  .reloading {
    position: absolute;
    top: 14px;
    inset-inline-end: 18px;
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
  }
  .caption {
    font-size: 12.5px;
    margin-top: 14px;
    max-width: 640px;
    line-height: 1.5;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1.4fr;
    gap: 18px;
    align-items: start;
  }
  .hint {
    font-size: 12px;
    margin-bottom: 12px;
  }
  .empty-inline {
    font-size: 13px;
  }
  .empty-inline.pad {
    padding: 18px;
  }
  .step-tag {
    margin-inline-start: 8px;
    font-size: 11px;
  }
  .arrow {
    text-align: center;
    font-size: 14px;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
