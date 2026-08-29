<script lang="ts">
  import { t, formatNumber } from '../lib/i18n';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import RetentionGrid from '../lib/components/RetentionGrid.svelte';
  import LifecycleChart from '../lib/components/LifecycleChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { getRetention, getLifecycle, getChurn } from '../lib/api/retention';
  import { gridToCsv } from '../lib/models/retention';
  import type { RetentionGrid as Grid, Granularity, ChurnPerson } from '../lib/models/retention';
  import type { LifecycleOut } from '../lib/api/retention';

  const BACKFILL_COMMAND = 'sauron-migrate backfill-person-days';

  let granularity = $state<Granularity>('day');
  let split = $state(false);

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
    sessionStore.scopeKey;
    if (!id) return;
    churnLoading = true;
    churnPeople = [];
    churnCursor = null;
    getChurn(id, { granularity: g, silent_periods: 4, limit: 25 })
      .then((r) => {
        churnPeople = r.people;
        churnSilentDays = r.silent_days;
        churnCursor = r.next_before;
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
        before: cursor,
      });
      churnPeople = [...churnPeople, ...r.people];
      churnCursor = r.next_before;
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

  const CHURN_COLUMNS = [
    { key: 'distinct_id', label: 'retention.churn.person' },
    { key: 'last_seen', label: 'retention.churn.lastSeen' },
    { key: 'events_count', label: 'retention.churn.events' },
  ];
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
        <RetentionGrid cohorts={grid.cohorts} {granularity} />
        {#if split && grid.clean}
          <h3 class="split-head">{t('retention.errorSplit.clean')}</h3>
          <RetentionGrid cohorts={grid.clean} {granularity} />
          <p class="caveat">{t('retention.errorSplit.caveat')}</p>
        {/if}
      {/if}
    </Card>

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
              {#each CHURN_COLUMNS as c (c.key)}
                <th scope="col">{t(c.label as never)}</th>
              {/each}
            </tr>
          {/snippet}
          {#snippet children()}
            {#each churnPeople as p (p.distinct_id)}
              <tr>
                <td>
                  <a href={`#/persons/${encodeURIComponent(p.distinct_id)}`}>{p.distinct_id}</a>
                </td>
                <td>{p.last_seen}</td>
                <td>{formatNumber(p.events_count)}</td>
              </tr>
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
</style>
