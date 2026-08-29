<script lang="ts">
  import { t, formatNumber } from '../lib/i18n';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
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
  // Percentages or absolute people. Page-owned so the error split's two grids
  // flip together; the header control is the accessible toggle, clicking any
  // cell is the shortcut.
  let gridMode = $state<GridMode>('rate');

  function toggleGridMode() {
    gridMode = gridMode === 'rate' ? 'count' : 'rate';
  }

  // Grid and lifecycle ride the house SWR view cache: at 51k persons the grid
  // is ~0.9 s and lifecycle ~1.8 s server-side, so returning to this page
  // repaints the cached answer instantly and refreshes behind the
  // "Updating…" indicator. Keys are built from the explicit inputs — appId,
  // scopeKey, granularity, split — NEVER from the clock; a clock-derived key
  // mints a fresh entry per load and hits zero times while every test stays
  // green.
  const gridView = new CachedView<Grid>();
  const lifeView = new CachedView<LifecycleOut>();

  const grid = $derived(gridView.data ?? null);
  const gridLoading = $derived(gridView.loading);
  const gridRevalidating = $derived(gridView.revalidating);
  const gridError = $derived(gridView.error);
  const life = $derived(lifeView.data ?? null);
  const lifeLoading = $derived(lifeView.loading);

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
    // Touch scopeKey so the effect re-runs when the environment picker
    // changes. The axios client injects `environment_id` itself, so nothing
    // below reads this value — without the touch the request would simply
    // never be re-sent and the grid would keep showing the previous
    // environment's numbers.
    const scope = sessionStore.scopeKey;
    if (!id) return;
    void gridView.load(viewKey('retention.grid', id, scope, g, sp), () =>
      getRetention(id, { granularity: g, cohorts: 12, periods: 12, split: sp ? 'errors' : 'none' }),
    );
  });

  $effect(() => {
    const id = appId;
    const g = granularity;
    const scope = sessionStore.scopeKey;
    if (!id) return;
    void lifeView.load(viewKey('retention.lifecycle', id, scope, g), () =>
      getLifecycle(id, { granularity: g, periods: 12 }),
    );
  });

  $effect(() => {
    const id = appId;
    const g = granularity;
    const sort = sortParam(churnSort);
    sessionStore.scopeKey;
    if (!id) return;
    churnLoading = true;
    churnPeople = [];
    churnCursor = null;
    expandedPerson = null;
    getChurn(id, { granularity: g, silent_periods: 4, limit: 25, sort })
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

<div class="page">
  <header class="head">
    <div>
      <h1>{t('retention.title')}</h1>
      <p class="sub">{t('retention.subtitle')}</p>
    </div>
    <div class="controls">
      <RollupChip />
      {#if gridRevalidating}<span class="updating">{t('retention.updating')}</span>{/if}
      <div class="seg-group" role="group">
        <button
          type="button"
          class:active={granularity === 'day'}
          onclick={() => (granularity = 'day')}
        >
          {t('retention.granularity.day')}
        </button>
        <button
          type="button"
          class:active={granularity === 'week'}
          onclick={() => (granularity = 'week')}
        >
          {t('retention.granularity.week')}
        </button>
      </div>
      <label class="split-toggle">
        <input type="checkbox" bind:checked={split} />
        {t('retention.errorSplit.toggle')}
      </label>
    </div>
  </header>

  {#if notReady}
    <Card>
      <h2 class="not-ready-title">{t('retention.notReady.title')}</h2>
      <p>{t('retention.notReady.body')}</p>
      <CodeBlock code={BACKFILL_COMMAND} language="bash" />
    </Card>
  {:else}
    <Card title={t('retention.title')}>
      {#snippet actions()}
        <div class="seg-group grid-mode" role="group" aria-label={t('retention.mode.label')}>
          <button
            type="button"
            class="seg"
            class:active={gridMode === 'rate'}
            aria-pressed={gridMode === 'rate'}
            onclick={() => (gridMode = 'rate')}
          >
            %
          </button>
          <button
            type="button"
            class="seg"
            class:active={gridMode === 'count'}
            aria-pressed={gridMode === 'count'}
            title={t('retention.mode.countTitle')}
            onclick={() => (gridMode = 'count')}
          >
            #
          </button>
        </div>
        <Button
          size="sm"
          variant="ghost"
          disabled={!grid || grid.cohorts.length === 0}
          onclick={exportCsv}
        >
          {t('retention.export')}
        </Button>
      {/snippet}
      {#if gridLoading}
        <Skeleton rows={6} />
      {:else if gridError}
        <EmptyState title={String(gridError)} />
      {:else if !grid || grid.cohorts.length === 0}
        <EmptyState title={t('retention.empty.title')} description={t('retention.empty.body')} />
      {:else}
        {#if split}
          <h3 class="split-head">{t('retention.errorSplit.exposed')}</h3>
        {/if}
        <RetentionGrid
          cohorts={grid.cohorts}
          {granularity}
          mode={gridMode}
          onmodetoggle={toggleGridMode}
        />
        {#if split && grid.clean}
          <h3 class="split-head">{t('retention.errorSplit.clean')}</h3>
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
      {:else if !life || life.points.length === 0}
        <EmptyState title={t('retention.empty.title')} description={t('retention.empty.body')} />
      {:else}
        <p class="sub">{t('retention.lifecycle.subtitle')}</p>
        <LifecycleChart points={life.points} />
      {/if}
    </Card>

    <Card title={t('retention.churn.title')}>
      {#if churnLoading}
        <Skeleton rows={4} />
      {:else if churnPeople.length === 0}
        <EmptyState title={t('retention.empty.title')} description={t('retention.empty.body')} />
      {:else}
        <p class="sub">
          {t('retention.churn.subtitle', { days: String(churnSilentDays) })}
        </p>
        <DataTable>
          {#snippet head()}
            <tr>
              <th scope="col">{t('retention.churn.person')}</th>
              <SortableTh key="last_seen" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.lastSeen')}
              </SortableTh>
              <SortableTh key="events" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.events')}
              </SortableTh>
              <SortableTh key="errors" sort={churnSort} onsort={onChurnSort}>
                {t('retention.churn.errors')}
              </SortableTh>
              <SortableTh key="sessions" sort={churnSort} onsort={onChurnSort}>
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
                <td>{p.last_seen}</td>
                <td>{formatNumber(p.events_count)}</td>
                <td>{formatNumber(p.errors_count)}</td>
                <td>{formatNumber(p.sessions_count)}</td>
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
                        <span class="detail-value">{p.first_seen.slice(0, 10)}</span>
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
    gap: 16px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: flex-start;
    justify-content: space-between;
  }

  h1 {
    margin: 0;
    font-size: 1.25rem;
  }

  .sub {
    margin: 4px 0 0;
    color: var(--muted-fg);
    font-size: 0.8125rem;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .seg-group {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }

  .seg-group button {
    background: transparent;
    border: 0;
    padding: 5px 12px;
    font: inherit;
    font-size: 0.8125rem;
    color: var(--muted-fg);
    cursor: pointer;
  }

  .seg-group button.active {
    background: var(--primary);
    color: var(--primary-fg, #fff);
  }

  .split-toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 0.8125rem;
    color: var(--muted-fg);
  }

  .split-head {
    margin: 12px 0 6px;
    font-size: 0.8125rem;
    color: var(--muted-fg);
    font-weight: 500;
  }

  .caveat {
    margin: 10px 0 0;
    font-size: 0.75rem;
    color: var(--muted-fg);
  }

  .not-ready-title {
    margin: 0 0 6px;
    font-size: 1rem;
  }

  .updating {
    font-size: 0.75rem;
    color: var(--muted-fg);
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
    font-size: 0.875rem;
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
    font-size: 0.8125rem;
    color: var(--muted-fg);
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
    background: var(--muted-fg);
  }

  .insights li[data-tone='bad'] .dot {
    background: var(--danger);
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

  .churn-detail td {
    background: var(--muted);
  }

  .detail-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 28px;
    align-items: baseline;
    padding: 4px 0;
    font-size: 0.8125rem;
  }

  .detail-label {
    color: var(--muted-fg);
    margin-inline-end: 6px;
  }

  .detail-value {
    font-variant-numeric: tabular-nums;
  }

  .detail-value[data-tone='bad'] {
    color: var(--danger);
  }
</style>
