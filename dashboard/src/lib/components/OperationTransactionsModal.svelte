<script lang="ts">
  import { t, formatNumber } from '../i18n';
  /**
   * The individual spans behind one row of the Performance "Operations" table.
   *
   * The Performance table is an AGGREGATE — `performance_summary` groups by
   * `(name, op)` and returns percentiles. The question it provokes ("p99 is
   * 4 s — which calls were those?") had no answer short of retyping the
   * operation name into the Transactions page's filter box. This is that
   * answer, one click away, filtered to exactly the group that was clicked.
   *
   * `name` AND `op` both go into the filter, never `name` alone: the endpoint
   * groups by the pair, so two rows can share a name under different ops and
   * filtering on the name would silently merge them — the modal would show
   * more spans than the row it was opened from counted.
   */
  import Modal from './ui/Modal.svelte';
  import Badge from './ui/Badge.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import Spinner from './ui/Spinner.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import DataTable from './DataTable.svelte';
  import TimeValue from './TimeValue.svelte';
  import LatencyBadge from './LatencyBadge.svelte';
  import TransactionDetailPanel from './TransactionDetailPanel.svelte';
  import { encodeFilters, type Filter } from './filters/filters';
  import { sessionStore } from '../stores/session.svelte';
  import { rangeKey, toParams, toPredicate, type DateRangeValue } from '../models/date-range';
  import { listTransactions } from '../api/transactions';
  import type { SearchEnvelope } from '../api/search';
  import { errorMessage } from '../api/client';
  import { httpStatusTone } from '../models/timeline-row';
  import type { PerfSummaryRow, Transaction } from '../models';

  /** One screenful. Deeper walks belong on the Transactions page, which pages. */
  const LIMIT = 50;

  interface Props {
    open: boolean;
    /** The clicked aggregate row; `null` whenever the modal is closed. */
    row: PerfSummaryRow | null;
    appId: string | null;
    /** The window the Performance page is showing, so both agree. */
    range: DateRangeValue;
    onclose?: () => void;
  }

  let { open = $bindable(false), row, appId, range, onclose }: Props = $props();

  /**
   * Slowest-first by default, not newest-first.
   *
   * The Transactions page opens on `occurred_at` because "what happened
   * recently" is its question. This modal is opened FROM a percentile column,
   * so the spans that produced that number are the ones to show first. A
   * `-` prefix means ascending on this route, so a bare `duration_ms` is
   * descending — slowest at the top.
   */
  type SortMode = 'duration_ms' | 'occurred_at';
  let sortMode = $state<SortMode>('duration_ms');

  let page = $state.raw<SearchEnvelope<Transaction> | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let expanded = $state(new Set<string>());

  const rows = $derived(page?.data ?? []);
  const total = $derived(page?.total ?? 0);
  const totalCapped = $derived(page?.total_is_capped ?? false);

  /**
   * The chips that isolate this operation, in the same encoding the
   * Transactions page parses back. Built with `encodeFilters` rather than by
   * hand so the modal's request and the "View all" link can never disagree
   * about escaping — an operation name with a space or a colon in it is the
   * case that catches a hand-rolled version.
   */
  const filters = $derived<Filter[]>(
    row ? [
      { field: 'name', op: 'eq', value: row.name },
      { field: 'op', op: 'eq', value: row.op },
    ] : [],
  );
  const encoded = $derived(encodeFilters(filters));

  /** Deep link to the full, pageable list with the same predicate and window. */
  const allHref = $derived.by(() => {
    if (!row) return '#/transactions';
    const p = new URLSearchParams();
    for (const f of encoded) p.append('filter', f);
    for (const [k, v] of Object.entries(toParams(range))) p.set(k, v);
    p.set('sort', sortMode === 'duration_ms' ? 'duration_ms' : 'occurred_at');
    return `#/transactions?${p.toString()}`;
  });

  let gen = 0;

  async function load() {
    if (!appId || !row) return;
    const myGen = ++gen;
    loading = true;
    error = null;
    try {
      const envelope = await listTransactions(appId, {
        filters: encoded,
        ...toPredicate(range),
        limit: LIMIT,
        sort: sortMode,
      });
      // A response for a row the reader has already navigated away from must
      // not paint: the modal is reopened per click, so a slow first request can
      // land after a second operation's fast one.
      if (myGen !== gen) return;
      page = envelope;
    } catch (err) {
      if (myGen !== gen) return;
      error = errorMessage(err);
      page = null;
    } finally {
      if (myGen === gen) loading = false;
    }
  }

  /**
   * Fetch on open, and on any change of what is being asked for.
   *
   * The previous row's spans are cleared FIRST rather than left on screen
   * under the new title. This component is reused across clicks — it is not
   * remounted — so without the reset, opening operation B shows operation A's
   * table for as long as B's request is in flight, captioned as B.
   */
  $effect(() => {
    const isOpen = open;
    const r = row;
    const aid = appId;
    const days = rangeKey(range);
    const mode = sortMode;
    // Touch scopeKey so a mid-view environment switch refetches; the axios
    // interceptor supplies the value but nothing here would notice it changed.
    sessionStore.scopeKey;
    if (!isOpen || !r || !aid) return;
    void days;
    void mode;
    page = null;
    expanded = new Set();
    void load();
  });

  function toggle(id: string) {
    const next = new Set(expanded);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    expanded = next;
  }

  function opLabel(o: string): string {
    return o.replace('_', ' ');
  }
</script>

<Modal bind:open size="xl" {onclose} title={row ? row.name : 'Transactions'}>
  {#if row}
    <div class="bar">
      <div class="ctx">
        <Badge tone="neutral" size="sm">{opLabel(row.op)}</Badge>
        <span class="stat">p95 <strong><LatencyBadge ms={row.p95} size="sm" /></strong></span>
        <span class="stat">
          {#if loading && rows.length === 0}
            Loading…
          {:else if rows.length > 0}
            Showing {rows.length} of {formatNumber(total)}{totalCapped ? '+' : ''}
          {/if}
        </span>
      </div>
      <div class="modes" role="tablist" aria-label={t('ui.opModal.order')}>
        <button
          class="mode"
          class:active={sortMode === 'duration_ms'}
          type="button"
          role="tab"
          aria-selected={sortMode === 'duration_ms'}
          onclick={() => (sortMode = 'duration_ms')}
        >{t('ui.opModal.slowest')}</button>
        <button
          class="mode"
          class:active={sortMode === 'occurred_at'}
          type="button"
          role="tab"
          aria-selected={sortMode === 'occurred_at'}
          onclick={() => (sortMode = 'occurred_at')}
        >{t('ui.opModal.mostRecent')}</button>
      </div>
    </div>

    {#if loading && rows.length === 0}
      <div class="center"><Spinner size={22} /></div>
    {:else if error}
      <EmptyState title={t('ui.opModal.loadError')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={load}>{t('ui.tryAgain')}</Button>
        {/snippet}
      </EmptyState>
    {:else if rows.length === 0}
      <EmptyState
        title={t('ui.opModal.emptyTitle')}
        description={t('ui.opModal.emptyBody')}
        icon="timer"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th class="chev" aria-label={t('ui.opModal.expand')}></th>
            <th class="num">{t('ui.opModal.duration')}</th>
            <th>{t('common.status')}</th>
            <th>HTTP</th>
            <th>{t('ui.opModal.session')}</th>
            <th>{t('ui.opModal.when')}</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as tx (tx.id)}
            <!--
              The whole row toggles the detail, which is the reason to be here —
              so the chevron is an affordance rather than the only target. The
              session link inside stops propagation so it still navigates.
            -->
            <tr class="clickable" onclick={() => toggle(tx.id)}>
              <td class="chev">
                <button
                  class="chev-btn"
                  type="button"
                  aria-expanded={expanded.has(tx.id)}
                  aria-label={expanded.has(tx.id) ? 'Hide details' : 'Show details'}
                  onclick={(e) => { e.stopPropagation(); toggle(tx.id); }}
                >
                  <Icon name={expanded.has(tx.id) ? 'chevron-down' : 'chevron-right'} size={14} />
                </button>
              </td>
              <td class="num"><LatencyBadge ms={tx.duration_ms} size="sm" /></td>
              <td>
                {#if tx.status}
                  <Badge tone={tx.status === 'ok' ? 'success' : 'warning'} size="sm">{tx.status}</Badge>
                {:else}
                  <span class="muted">—</span>
                {/if}
              </td>
              <td>
                {#if tx.http_status != null}
                  <Badge tone={httpStatusTone(tx.http_status)} size="sm">
                    {tx.http_method ?? ''}
                    {tx.http_status}
                  </Badge>
                {:else if tx.http_method}
                  <Badge tone="neutral" size="sm">{tx.http_method}</Badge>
                {:else}
                  <span class="muted">—</span>
                {/if}
              </td>
              <td>
                {#if tx.session_id}
                  <a
                    class="session mono truncate"
                    href={`#/sessions/${encodeURIComponent(tx.session_id)}`}
                    title={tx.session_id}
                    onclick={(e) => e.stopPropagation()}
                  >{tx.session_id}</a>
                {:else}
                  <span class="muted" title={t('ui.opModal.noSession')}>—</span>
                {/if}
              </td>
              <td><TimeValue value={tx.occurred_at} /></td>
            </tr>
            {#if expanded.has(tx.id)}
              <tr class="meta-row">
                <!-- `wrap`: DataTable's blanket `white-space: nowrap` would
                     otherwise suppress every line break in the panel. -->
                <td class="wrap" colspan="6">
                  <TransactionDetailPanel transaction={tx} />
                </td>
              </tr>
            {/if}
          {/each}
        {/snippet}
      </DataTable>
    {/if}
  {/if}

  {#snippet footer()}
    <a class="all-link" href={allHref}>
      {t('ui.opModal.openInTransactions')}
      <Icon name="arrow-right" size={14} />
    </a>
    <Button variant="secondary" onclick={() => (open = false)}>{t('common.close')}</Button>
  {/snippet}
</Modal>

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 14px;
  }
  .ctx {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .stat {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    color: var(--text-muted);
  }
  .modes {
    display: inline-flex;
    gap: 2px;
    padding: 3px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .mode {
    padding: 4px 10px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12px;
    font-weight: 560;
    border-radius: var(--radius-sm);
    white-space: nowrap;
    cursor: pointer;
  }
  .mode:hover {
    color: var(--text);
  }
  .mode.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .center {
    display: flex;
    justify-content: center;
    padding: 40px 0;
  }
  .chev {
    width: 28px;
    padding-inline-end: 0;
  }
  .chev-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--muted);
    cursor: pointer;
  }
  .chev-btn:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .meta-row > td {
    background: var(--surface-2);
    padding: 12px 16px 16px;
  }
  .session {
    display: inline-block;
    max-width: 200px;
    vertical-align: bottom;
  }
  .all-link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-inline-end: auto;
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
  }
  .all-link:hover {
    color: var(--primary);
  }
</style>
