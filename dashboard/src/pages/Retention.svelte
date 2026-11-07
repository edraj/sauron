<script lang="ts">
  import { t, formatNumber } from '../lib/i18n';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import SegmentedControl from '../lib/components/ui/SegmentedControl.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import RetentionGrid from '../lib/components/RetentionGrid.svelte';
  import LifecycleChart from '../lib/components/LifecycleChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { getRetention, getLifecycle, getChurn } from '../lib/api/retention';
  import { gridToCsv } from '../lib/models/retention';
  import { insightLink, retentionInsights } from '../lib/models/retention-insights';
  import { canAccessPage, resolvePageAccess } from '../lib/models/page-access';
  import { sortParam, toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import type {
    RetentionGrid as Grid,
    Granularity,
    GridMode,
    ChurnPerson,
  } from '../lib/models/retention';
  import type { LifecycleOut } from '../lib/api/retention';

  const BACKFILL_COMMAND = 'sauron-migrate backfill-person-days';

  let granularity = $state<Granularity>('day');
  let split = $state(false);
  /**
   * Restrict every card to people the app has named.
   *
   * Defaults OFF deliberately: "did the people who arrived come back?" is a
   * question about everyone, and guests are most of the arrivals. Filtering
   * them out by default would quietly change what this page claims to measure
   * — and it selects for people who already converted, which flatters the
   * number. It is offered because an identified id comes from the app and is
   * stable even where an anonymous one is not.
   */
  let identifiedOnly = $state(false);
  // Percentages or absolute people. Page-owned so the error split's two grids
  // flip together; the header control is the accessible toggle, clicking any
  // cell is the shortcut.
  let gridMode = $state<GridMode>('rate');

  function toggleGridMode() {
    gridMode = gridMode === 'rate' ? 'count' : 'rate';
  }

  const granularityOptions = $derived([
    { value: 'day', label: t('retention.granularity.day') },
    { value: 'week', label: t('retention.granularity.week') },
  ]);
  // `%` and `#` are the cell values themselves, not words; the count segment
  // keeps its explanatory tooltip.
  const gridModeOptions = $derived([
    { value: 'rate', label: '%' },
    { value: 'count', label: '#', title: t('retention.mode.countTitle') },
  ]);

  // Grid and lifecycle ride the house SWR view cache: at 51k persons the grid
  // is ~0.9 s and lifecycle ~1.8 s server-side, so returning to this page
  // repaints the cached answer instantly and refreshes behind the
  // "Updating…" indicator. Keys are built from the explicit inputs — appId,
  // scopeKey, granularity, split — NEVER from the clock; a clock-derived key
  // mints a fresh entry per load and hits zero times while every test stays
  // green.
  const gridView = new CachedView<Grid>();
  const lifeView = new CachedView<LifecycleOut>();

  // Both views. The grid and the lifecycle chart answer the same question at
  // different resolutions, and refreshing one without the other would show two
  // views of retention that disagree.
  const refresher = pageRefresher(async () => {
    await Promise.all([gridView.reload(), lifeView.reload()]);
  });


  const grid = $derived(gridView.data ?? null);
  const gridLoading = $derived(gridView.loading);
  const gridRevalidating = $derived(gridView.revalidating);
  const gridError = $derived(gridView.error);
  const life = $derived(lifeView.data ?? null);
  const lifeLoading = $derived(lifeView.loading);
  const lifeError = $derived(lifeView.error);

  // Churn accumulates pages through a cursor, which a keyed whole-view cache
  // models badly — and at ~40 ms server-side it does not need one.
  let churnPeople = $state<ChurnPerson[]>([]);
  let churnSilentDays = $state(0);
  let churnCursor = $state<string | null>(null);
  let churnLoading = $state(true);
  let churnMoreLoading = $state(false);
  // Server-side sort — the silent population is far larger than one page, so
  // sorting only the loaded rows would silently lie. State change refetches
  // from the top; the cursor is bound to the sort that minted it.
  let churnSort = $state<SortState>({ key: 'last_seen', dir: 'desc' });
  /** The one expanded row's person id, or null. */
  let expandedPerson = $state<string | null>(null);

  function onChurnSort(key: string, columnDefault: SortDir) {
    churnSort = toggleSort(churnSort, key, columnDefault);
  }

  /**
   * Row click toggles the detail panel — EXCEPT clicks on the person link,
   * which navigate. `closest('a')` rather than target identity so a click on
   * the link's text node still counts as the link.
   */
  function toggleExpanded(e: MouseEvent, id: string) {
    if ((e.target as Element | null)?.closest('a')) return;
    expandedPerson = expandedPerson === id ? null : id;
  }

  /** Whole days between an ISO instant and now, for the detail panel. */
  function daysSince(iso: string): number {
    return Math.max(0, Math.floor((Date.now() - new Date(iso).getTime()) / 86_400_000));
  }

  /** Whole days between two ISO instants — the person's active tenure. */
  function daysBetween(fromIso: string, toIso: string): number {
    return Math.max(
      0,
      Math.round((new Date(toIso).getTime() - new Date(fromIso).getTime()) / 86_400_000),
    );
  }

  const appId = $derived(sessionStore.currentAppId);

  /**
   * `ready === false` is a first-class state, not an empty grid.
   *
   * It means the operator has not run the one-time backfill, so this app's
   * history simply is not in `person_days` yet. Rendering an empty grid there
   * would be a confident 0% — indistinguishable from "everyone churned".
   */
  const notReady = $derived(grid !== null && !grid.ready);

  $effect(() => {
    const id = appId;
    const g = granularity;
    const sp = split;
    const idOnly = identifiedOnly;
    // Touch scopeKey so the effect re-runs when the environment picker
    // changes. The axios client injects `environment_id` itself, so nothing
    // below reads this value — without the touch the request would simply
    // never be re-sent and the grid would keep showing the previous
    // environment's numbers.
    const scope = sessionStore.scopeKey;
    if (!id) return;
    // `idOnly` is part of the KEY, not just the request: two entries whose
    // only difference is the audience must not share one slot, or a cached
    // all-users grid gets shown under the identified-only label.
    void gridView.load(viewKey('retention.grid', id, scope, g, sp, idOnly), () =>
      getRetention(id, {
        granularity: g,
        cohorts: 12,
        periods: 12,
        split: sp ? 'errors' : 'none',
        identified_only: idOnly,
      }),
    );
  });

  $effect(() => {
    const id = appId;
    const g = granularity;
    const idOnly = identifiedOnly;
    const scope = sessionStore.scopeKey;
    if (!id) return;
    void lifeView.load(viewKey('retention.lifecycle', id, scope, g, idOnly), () =>
      getLifecycle(id, { granularity: g, periods: 12, identified_only: idOnly }),
    );
  });

  $effect(() => {
    const id = appId;
    const g = granularity;
    const sort = sortParam(churnSort);
    const idOnly = identifiedOnly;
    sessionStore.scopeKey;
    if (!id) return;
    churnLoading = true;
    churnPeople = [];
    churnCursor = null;
    expandedPerson = null;
    getChurn(id, { granularity: g, silent_periods: 4, limit: 25, sort, identified_only: idOnly })
      .then((r) => {
        churnPeople = r.people;
        churnSilentDays = r.silent_days;
        churnCursor = r.next_cursor;
      })
      .catch(() => {
        churnPeople = [];
        churnCursor = null;
      })
      .finally(() => {
        churnLoading = false;
      });
  });

  async function loadMoreChurn() {
    const id = appId;
    const cursor = churnCursor;
    if (!id || !cursor || churnMoreLoading) return;
    churnMoreLoading = true;
    try {
      const r = await getChurn(id, {
        granularity,
        silent_periods: 4,
        limit: 25,
        sort: sortParam(churnSort),
        cursor,
        identified_only: identifiedOnly,
      });
      churnPeople = [...churnPeople, ...r.people];
      churnCursor = r.next_cursor;
    } catch {
      // Keep what we have; the button stays for a retry.
    } finally {
      churnMoreLoading = false;
    }
  }

  /** Raw counts, so a spreadsheet can derive any rate — see gridToCsv. */
  function exportCsv() {
    if (!grid || grid.cohorts.length === 0) return;
    const csv = gridToCsv(grid.cohorts, granularity);
    const blob = new Blob([csv], { type: 'text/csv;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `retention-${granularity}.csv`;
    a.click();
    URL.revokeObjectURL(url);
  }

  /**
   * The computed reading of what is on screen. Derived from the SAME loaded
   * responses the two charts render, so the sentences and the pixels cannot
   * disagree; empty until both cards have data.
   */
  const insights = $derived(
    grid && grid.ready && life && life.ready
      ? retentionInsights(grid.cohorts, life.points)
      : [],
  );

</script>

<PageHeader title={t('retention.title')} subtitle={t('retention.subtitle')}>
  {#snippet freshness()}
    <RollupChip />
    {#if gridRevalidating}<span class="updating">{t('retention.updating')}</span>{/if}
  {/snippet}
  {#snippet refresh()}
    <RefreshButton
      onclick={refresher.run}
      loading={refresher.busy || gridRevalidating || lifeView.revalidating}
    />
  {/snippet}
  {#snippet actions()}
    {#if !notReady}
      <Button
        variant="secondary"
        disabled={!grid || grid.cohorts.length === 0}
        onclick={exportCsv}
      >
        <Icon name="download" size={15} />
        {t('retention.export')}
      </Button>
    {/if}
  {/snippet}
</PageHeader>

<div class="page">
  <div class="filters">
    <SegmentedControl
      value={granularity}
      options={granularityOptions}
      ariaLabel={t('retention.granularity.label')}
      onchange={(v) => (granularity = v as Granularity)}
    />
    <label class="check">
      <input type="checkbox" bind:checked={split} />
      {t('retention.errorSplit.toggle')}
    </label>
    <label class="check" title={t('retention.identifiedOnly.hint')}>
      <input type="checkbox" bind:checked={identifiedOnly} />
      {t('retention.identifiedOnly.toggle')}
    </label>
  </div>

  {#if notReady}
    <Card title={t('retention.notReady.title')}>
      <p>{t('retention.notReady.body')}</p>
      <p class="note">{t('retention.notReady.allApps')}</p>
      <CodeBlock code={BACKFILL_COMMAND} language="bash" />
    </Card>
  {:else}
    <Card title={t('retention.card.cohorts')}>
      {#snippet actions()}
        <SegmentedControl
          size="sm"
          value={gridMode}
          options={gridModeOptions}
          ariaLabel={t('retention.mode.label')}
          onchange={(v) => (gridMode = v as GridMode)}
        />
      {/snippet}
      {#if gridLoading}
        <Skeleton rows={6} />
      {:else if gridError}
        <ErrorState
          title={t('retention.error.grid')}
          error={gridView}
          onretry={() => void gridView.reload()}
          size="section"
        />
      {:else if !grid || grid.cohorts.length === 0}
        <EmptyState
          size="section"
          title={t('common.empty.range', { things: t('retention.noun') })}
          description={t('retention.empty.body')}
        />
      {:else}
        {#if split}
          <p class="section-label split-head">{t('retention.errorSplit.exposed')}</p>
        {/if}
        <RetentionGrid
          cohorts={grid.cohorts}
          {granularity}
          mode={gridMode}
          onmodetoggle={toggleGridMode}
        />
        {#if split && grid.clean}
          <p class="section-label split-head">{t('retention.errorSplit.clean')}</p>
          <RetentionGrid
            cohorts={grid.clean}
            {granularity}
            mode={gridMode}
            onmodetoggle={toggleGridMode}
          />
          <p class="caveat">{t('retention.errorSplit.caveat')}</p>
        {/if}
      {/if}
    </Card>

    {#if insights.length > 0}
      <Card title={t('retention.insights.title')}>
        <ul class="insights">
          {#each insights as ins (ins.key)}
            {@const link = insightLink(ins, (r) => canAccessPage(resolvePageAccess(r)))}
            <li data-tone={ins.tone}>
              <span class="dot" aria-hidden="true"></span>
              <div class="insight-body">
                <p class="finding">{t(ins.key as never, ins.params)}</p>
                <p class="action">
                  {t(ins.actionKey as never)}
                  {#if link}
                    <a href={`#${link.route}`}>{t(link.labelKey as never)} &rarr;</a>
                  {/if}
                </p>
              </div>
            </li>
          {/each}
        </ul>
      </Card>
    {/if}

    <Card title={t('retention.lifecycle.title')}>
      {#if lifeLoading}
        <Skeleton rows={4} />
      {:else if lifeError && !life}
        <ErrorState
          title={t('retention.error.lifecycle')}
          error={lifeView}
          onretry={() => void lifeView.reload()}
          size="section"
        />
      {:else if !life || life.points.length === 0}
        <EmptyState
          size="section"
          title={t('common.empty.range', { things: t('retention.noun') })}
          description={t('retention.empty.body')}
        />
      {:else}
        <p class="note">{t('retention.lifecycle.subtitle')}</p>
        <LifecycleChart points={life.points} />
      {/if}
    </Card>

    <Card title={t('retention.churn.title')}>
      {#if churnLoading}
        <Skeleton rows={4} />
      {:else if churnPeople.length === 0}
        <EmptyState size="section" title={t('retention.churn.empty')} />
      {:else}
        <p class="note">
          {t('retention.churn.subtitle', { days: String(churnSilentDays) })}
        </p>
        <DataTable>
          {#snippet head()}
            <tr>
              <th scope="col">{t('retention.churn.person')}</th>
              <SortableTh key="last_seen" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.lastSeen')}
              </SortableTh>
              <SortableTh key="events" class="num" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.events')}
              </SortableTh>
              <SortableTh key="errors" class="num" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.errors')}
              </SortableTh>
              <SortableTh key="sessions" class="num" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.sessions')}
              </SortableTh>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each churnPeople as p (p.distinct_id)}
              {@const open = expandedPerson === p.distinct_id}
              <!-- The row toggles the detail panel; the person link inside it
                   navigates (toggleExpanded ignores clicks landing on the
                   anchor, and middle-click stays native to the <a>). -->
              <tr
                class="churn-row"
                class:open
                aria-expanded={open}
                onclick={(e) => toggleExpanded(e, p.distinct_id)}
              >
                <td>
                  <a class="person-link" href={`#/persons/${encodeURIComponent(p.distinct_id)}`}>
                    {p.distinct_id}
                  </a>
                </td>
                <td><TimeValue value={p.last_seen} muted /></td>
                <td class="num">{formatNumber(p.events_count)}</td>
                <td class="num">{formatNumber(p.errors_count)}</td>
                <td class="num">{formatNumber(p.sessions_count)}</td>
              </tr>
              {#if open}
                <tr class="churn-detail">
                  <td colspan="5">
                    <div class="detail-grid">
                      <div>
                        <span class="detail-label">{t('retention.churn.silentFor')}</span>
                        <span class="detail-value" data-tone="bad">
                          {t('retention.churn.nDays', { n: formatNumber(daysSince(p.last_seen)) })}
                        </span>
                      </div>
                      <div>
                        <span class="detail-label">{t('retention.churn.tenure')}</span>
                        <span class="detail-value">
                          {t('retention.churn.nDays', {
                            n: formatNumber(daysBetween(p.first_seen, p.last_seen)),
                          })}
                        </span>
                      </div>
                      <div>
                        <span class="detail-label">{t('retention.churn.firstSeen')}</span>
                        <span class="detail-value"><TimeValue value={p.first_seen} /></span>
                      </div>
                      <div>
                        <span class="detail-label">{t('retention.churn.errors')}</span>
                        <span class="detail-value" data-tone={p.errors_count > 0 ? 'bad' : undefined}>
                          {formatNumber(p.errors_count)}
                        </span>
                      </div>
                      <div>
                        <span class="detail-label">{t('retention.churn.sessions')}</span>
                        <span class="detail-value">{formatNumber(p.sessions_count)}</span>
                      </div>
                      <a class="person-link" href={`#/persons/${encodeURIComponent(p.distinct_id)}`}>
                        {t('retention.churn.viewProfile')}
                      </a>
                    </div>
                  </td>
                </tr>
              {/if}
            {/each}
          {/snippet}
        </DataTable>
        {#if churnCursor}
          <div class="load-more">
            <Button size="sm" variant="ghost" loading={churnMoreLoading} onclick={loadMoreChurn}>
              {t('retention.churn.loadMore')}
            </Button>
          </div>
        {/if}
      {/if}
    </Card>
  {/if}
</div>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }

  .filters {
    display: flex;
    align-items: center;
    gap: 8px 16px;
    flex-wrap: wrap;
  }

  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  /* A muted lede inside a section, under its title. */
  .note {
    margin: 0 0 10px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }

  .split-head {
    margin: 12px 0 6px;
  }

  .caveat {
    margin: 10px 0 0;
    font-size: var(--text-xs);
    color: var(--text-muted);
  }

  .updating {
    font-size: var(--text-xs);
    color: var(--text-muted);
  }

  .load-more {
    display: flex;
    justify-content: center;
    margin-top: 10px;
  }

  .insights {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: var(--text-sm);
  }

  .insights li {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .insight-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .finding {
    margin: 0;
  }

  /* The recommendation reads as a subordinate clause of the finding above it,
     never as a second finding — muted and one step down in size. */
  .action {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .action a {
    color: var(--primary);
    text-decoration: none;
    white-space: nowrap;
  }

  .action a:hover {
    text-decoration: underline;
  }

  .insights .dot {
    flex: 0 0 8px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
  }

  .insights li[data-tone='bad'] .dot {
    background: var(--error);
  }

  .insights li[data-tone='warn'] .dot {
    background: var(--warning);
  }

  .insights li[data-tone='good'] .dot {
    background: var(--success);
  }

  .churn-row {
    cursor: pointer;
  }

  .person-link {
    color: var(--primary);
    text-decoration: none;
  }

  .person-link:hover {
    text-decoration: underline;
  }

  /* `--surface-2`, the same well as the other expanded rows (Transactions'
     detail row). This read `--muted`, an undefined token, and the token fix
     mapped it to a TEXT colour, which painted the panel a solid mid-grey. */
  .churn-detail td {
    background: var(--surface-2);
  }

  .detail-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 28px;
    align-items: baseline;
    padding: 4px 0;
    font-size: var(--text-sm);
  }

  .detail-label {
    color: var(--text-muted);
    margin-inline-end: 6px;
  }

  .detail-value {
    font-variant-numeric: tabular-nums;
  }

  .detail-value[data-tone='bad'] {
    color: var(--error);
  }
</style>
