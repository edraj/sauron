<script lang="ts">
  import { t } from '../lib/i18n';
  import { untrack } from 'svelte';
  import { querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import TransactionDetailPanel from '../lib/components/TransactionDetailPanel.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import CursorPagination from '../lib/components/CursorPagination.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import TimeFilter from '../lib/components/TimeFilter.svelte';
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
  import {
    TRANSACTION_FIELDS,
    encodeFilters,
    parseFilters,
    type Filter,
  } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listTransactions } from '../lib/api/transactions';
  import type { SearchEnvelope } from '../lib/api/search';
  import { errorMessage, errorStatus } from '../lib/api/client';
  import { cursorOf, emptyPage, offsetOf, pageKey, pageNumber } from '../lib/models/cursor-page';
  import { cursorGoTo, setCursorSort, type CursorListState } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import { fromParams, toParams, type TimeField, type TimeFilterState } from '../lib/models/time-filter';
  import { viewKey } from '../lib/stores/view-cache';
  import { httpStatusTone } from '../lib/models/timeline-row';
  import type { Transaction } from '../lib/models';

  const LIMIT = 50;

  /**
   * Both window columns the route accepts (`routes/transactions.rs:TIME_FIELDS`).
   *
   * Unlike the Events stream — which offers only `occurred_at` because
   * `analytics_events.received_at` carries no index — `transactions` is
   * partitioned on `occurred_at` and `received_at` is a plain column here too,
   * so a `received_at` window prunes no partitions. It is offered anyway
   * because "what ARRIVED in the last hour" is a genuinely different question
   * for a mobile SDK with a skewed clock or a long offline queue, and that is
   * exactly the case where a span's `occurred_at` is the untrustworthy one.
   */
  const TIME_FIELDS: TimeField[] = [
    { key: 'occurred_at', label: 'Occurred' },
    { key: 'received_at', label: 'Received' },
  ];
  const DEFAULT_TIME_FIELD = 'occurred_at';

  // Hydrate from the URL once, at init — not inside an effect, so this never
  // re-runs and never fights the sync effect below.
  const initial = new URLSearchParams($querystring ?? '');
  let filters = $state<Filter[]>(parseFilters(initial.getAll('filter'), TRANSACTION_FIELDS));
  let search = $state(initial.get('q') ?? '');
  // The reload effect depends on this, not on `search`, so free-text typing
  // doesn't fire a request on every keystroke. Chips apply immediately.
  let appliedSearch = $state(initial.get('q') ?? '');
  // Present only to satisfy FilterBar's bindable range control; this page's
  // window lives in `window` below, on the table's own card. Kept in sync so
  // the two never disagree on screen.
  let sinceDays = $state(Number(initial.get('since_days')) || 7);
  let window_ = $state<TimeFilterState>(fromParams(initial, TIME_FIELDS, DEFAULT_TIME_FIELD, 365));

  let list = $state<CursorListState>({
    page: emptyPage(),
    sort: { key: initial.get('sort')?.replace(/^-/, '') ?? 'occurred_at', dir: initial.get('sort')?.startsWith('-') ? 'asc' : 'desc' },
  });

  let page = $state.raw<SearchEnvelope<Transaction> | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let errorStatusCode = $state<number | null>(null);
  let refreshing = $state(false);
  /** Row ids whose extras panel is open. */
  let expanded = $state(new Set<string>());

  const rows = $derived(page?.data ?? []);
  const total = $derived(page?.total ?? null);
  const totalCapped = $derived(page?.total_is_capped ?? false);
  const nextCursor = $derived(page?.next_cursor ?? null);
  const clamped = $derived(page?.clamped ?? null);

  function toggle(id: string) {
    const next = new Set(expanded);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    expanded = next;
  }

  /** The inputs that produced `page`, as a `viewKey` string. See `load`'s catch. */
  let key = $state<string | null>(null);
  let gen = 0;

  async function load(
    appId: string,
    filterList: string[],
    q: string,
    tf: TimeFilterState,
    l: CursorListState,
  ) {
    const myGen = ++gen;
    const k = viewKey(
      'transactions.list',
      appId,
      // Carries the selected environment, which the axios interceptor adds to
      // the request but which appears in none of these other arguments. Omit
      // it and one environment's spans key the same as another's.
      sessionStore.scopeKey,
      filterList,
      q.trim(),
      // The window's DECLARATION, never the instant `last` resolves to: a
      // clock-derived key component mints a fresh entry on every load, so the
      // cache hits zero times while staying wired, typed and green.
      `${tf.field}:${tf.mode}:${tf.lastDays ?? ''}:${tf.from ?? ''}:${tf.to ?? ''}`,
      sortParam(l.sort),
      // `pageKey`, NOT `cursorOf`: a page reached by a numbered jump carries a
      // null cursor, which is exactly what page 1 carries — so a key built from
      // the cursor alone hashes page 7 to page 1's entry and repaints the first
      // page straight out of the cache, with no request on the wire to notice.
      pageKey(l.page),
    );
    loading = true;
    error = null;
    errorStatusCode = null;
    try {
      const envelope = await listTransactions(appId, {
        filters: filterList,
        // `query`, NOT `q`. The FilterBar's box is a query-LANGUAGE input —
        // its placeholder is generated from this resource's schema and reads
        // `extra:…, @tag.key:value…`. Sent as `q` it goes through the legacy
        // bridge, where `@tag.tier:premium` is one free-text term rather than
        // a predicate: the box would offer a syntax it then matched literally
        // and returned nothing for. `query` still accepts bare free text
        // (verified: `query=order_id` and `q=order_id` return the same row),
        // so nothing is lost by preferring it.
        query: q.trim() || undefined,
        // Sent only in `last` mode; `predicateParams` drops it whenever a
        // bound is present, so the two can never both reach the wire.
        sinceDays: tf.mode === 'last' ? tf.lastDays : undefined,
        timeField: tf.field === DEFAULT_TIME_FIELD ? undefined : tf.field,
        from: tf.from,
        to: tf.to,
        limit: LIMIT,
        sort: sortParam(l.sort),
        cursor: cursorOf(l.page),
        offset: offsetOf(l.page),
      });
      if (myGen !== gen) return;
      page = envelope;
      key = k;
      // A row's expansion is keyed on its id, and the ids on screen have just
      // been replaced. Left alone, an id reappearing on a later page would come
      // back already open — state from a view the reader has left.
      expanded = new Set();
    } catch (err) {
      if (myGen !== gen) return;
      error = errorMessage(err);
      errorStatusCode = errorStatus(err);
      // Keep the rows ONLY when the request that failed asked for exactly what
      // is on screen (a Refresh or a Retry). Then they are still a true answer,
      // merely older than asked for, and the banner says so. For any other key
      // they answer a DIFFERENT question, and leaving them under the new chips
      // would present them as the new result.
      if (key !== k) {
        page = null;
        key = null;
      }
    } finally {
      // Left to the newest call: a superseded one clearing this would drop the
      // spinner while its replacement is still in flight.
      if (myGen === gen) {
        loading = false;
        refreshing = false;
      }
    }
  }

  const staleError = $derived(error !== null && rows.length > 0);
  const fatalError = $derived(error !== null && rows.length === 0);

  /**
   * Prev/Next/sort load IMPERATIVELY rather than by writing state the reload
   * effect reads back. An effect that both wrote `list` and read it to build
   * its request would re-run on its own write.
   */
  function toPage(next: CursorListState) {
    const aid = sessionStore.currentAppId;
    list = next;
    page = null;
    if (aid) void load(aid, encodeFilters(filters), appliedSearch, window_, next);
  }

  /**
   * `columnDefault` is the direction a column sorts on its FIRST click.
   *
   * Text columns read naturally ascending; `duration_ms` and `occurred_at` do
   * not — the first thing anyone wants from either is the largest/most recent,
   * so those default to descending. Passed from the same table that declares
   * the header, so the header's arrow and the request agree.
   */
  function onsort(k: string) {
    const columnDefault: SortDir = k === 'name' || k === 'op' ? 'asc' : 'desc';
    toPage(setCursorSort(list, k, columnDefault));
  }

  /**
   * Move to a numbered page.
   *
   * `cursorGoTo` picks the mechanism — a keyset step when the target is
   * adjacent and a cursor for it exists, an offset jump otherwise. That choice
   * is made in one place for all four cursor lists rather than here, so no two
   * of them can page differently.
   */
  function onjump(target: number) {
    toPage(cursorGoTo(list, target, nextCursor, LIMIT));
  }

  function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    void load(aid, encodeFilters(filters), appliedSearch, window_, list);
  }

  /**
   * Commit the search box into `appliedSearch` on an explicit submit.
   *
   * **Not optional plumbing.** `search` is what the FilterBar binds and
   * `appliedSearch` is what the request reads; without this the two never
   * meet, and the search box types, validates and highlights while every
   * request goes out without a `q` at all — a control that is wired, typed and
   * green and does nothing.
   *
   * Deliberately a callback rather than an effect on `search`: nothing may
   * observe the typed text and requery, or the box is back to firing per
   * keystroke with a delay in front of it. Chips and the time filter still
   * reload immediately — they cannot be half-written the way a query can.
   */
  function onSearch(q: string) {
    appliedSearch = q;
  }

  // Predicate changes reset to page one — a cursor minted under one predicate
  // is not a position within another.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // axios interceptor supplies the value, but nothing would refetch without
    // this — one environment's spans would sit on screen labelled as another's.
    // It also has to RESET the walk, not merely refetch, which the `fresh`
    // page below already does: a cursor minted in one environment is not a
    // position within another.
    sessionStore.scopeKey;
    const f = encodeFilters(filters);
    const q = appliedSearch;
    const w = window_;
    if (!aid) return;
    // **`untrack`, and it is load-bearing.** This effect WRITES `list`, so a
    // tracked read of `list.sort` makes it depend on its own write. `$state`
    // deep-proxies, so the fresh object is never `===` the old one no matter
    // how identical its contents — the effect re-runs, writes again, and the
    // loop only ends when Svelte throws `effect_update_depth_exceeded`.
    // Observed: the page fired ~50 identical requests and then died.
    //
    // The sort genuinely belongs here (a predicate change must keep the
    // current ordering while resetting to page one), so the read stays and the
    // reactivity goes. Sorting reloads imperatively through `onsort`/`toPage`,
    // which is why this effect never needs to *observe* it.
    const fresh: CursorListState = { page: emptyPage(), sort: untrack(() => list.sort) };
    list = fresh;
    page = null;
    void load(aid, f, q, w, fresh);
  });

  // Mirror the predicate into the URL so a view is linkable and survives reload.
  $effect(() => {
    const p = toParams(window_, DEFAULT_TIME_FIELD);
    for (const f of encodeFilters(filters)) p.append('filter', f);
    if (appliedSearch.trim()) p.set('q', appliedSearch.trim());
    const s = sortParam(list.sort);
    if (s) p.set('sort', s);
    const qs = p.toString();
    replace(qs ? `/transactions?${qs}` : '/transactions');
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1>{t('transactions.title')}</h1>
      <p class="sub muted">
        {t('transactions.subtitle')}
        <a href="#/performance">{t('perf.title')}</a> {t('transactions.aggregatedBy')}
      </p>
    </div>
    <RefreshButton onclick={refresh} loading={refreshing} />
  </div>

  <FilterBar
    fields={TRANSACTION_FIELDS}
    bind:filters
    bind:search
    bind:sinceDays
    appId={sessionStore.currentAppId ?? undefined}
    context="transactions"
    error={errorStatusCode === 400 ? error : null}
    {onSearch}
    showRange={false}
  />
  <SearchDisclosure {clamped} />

  <Card padding="none" title={t('transactions.card.spans')}>
    {#snippet actions()}
      <TimeFilter fields={TIME_FIELDS} value={window_} onchange={(v) => (window_ = v)} />
    {/snippet}

    <!--
      A refresh or retry of THESE rows failed. They stay, and so does the pager
      — losing page 4 of a walk to one bad poll is a worse outcome than rows a
      minute old. `role="status"`, not `alert`: nothing on screen is broken.
    -->
    {#if staleError}
      <p class="stale-banner" role="status">
        <Icon name="triangle-alert" size={14} />
        <span>Showing the last results that loaded — refreshing failed: {error}</span>
        <Button variant="ghost" size="sm" onclick={refresh}>{t('ui.tryAgain')}</Button>
      </p>
    {/if}

    {#if loading && rows.length === 0}
      <div class="center"><Spinner size={22} /></div>
    {:else if fatalError}
      <EmptyState title={t('transactions.error.load')} description={error ?? undefined} icon="triangle-alert">
        {#snippet action()}
          <Button onclick={refresh}>{t('ui.tryAgain')}</Button>
        {/snippet}
      </EmptyState>
    {:else if rows.length === 0}
      <EmptyState
        title={t('transactions.empty.title')}
        description={t('transactions.empty.body')}
        icon="timer"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th class="chev" aria-label={t('ui.opModal.expand')}></th>
            <SortableTh key="name" columnDefault="asc" sort={list.sort} {onsort}>{t('common.name')}</SortableTh>
            <SortableTh key="op" columnDefault="asc" sort={list.sort} {onsort}>Op</SortableTh>
            <SortableTh key="duration_ms" class="num" sort={list.sort} {onsort}>{t('explore.column.duration')}</SortableTh>
            <th>{t('common.status')}</th>
            <th>HTTP</th>
            <!--
              Not a `SortableTh`. The backend's sort whitelist is
              occurred_at/duration_ms/name/op — the four orderings with a
              keyset index behind them — and asking for anything else is a 400
              that names the allowed set. A header that looks clickable and
              answers with an error is worse than one that plainly does not
              sort. Narrow instead: click the id, or filter `session:<id>`.
            -->
            <th>{t('sessions.column.session')}</th>
            <SortableTh key="occurred_at" sort={list.sort} {onsort}>{t('ui.opModal.when')}</SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as tx (tx.id)}
            <tr class:expanded={expanded.has(tx.id)}>
              <td class="chev">
                <!--
                  Unconditional. It used to appear only on rows carrying
                  tags/extra, back when expanding showed nothing else — now it
                  opens the full span, which every row has.
                -->
                <button
                  class="chev-btn"
                  aria-expanded={expanded.has(tx.id)}
                  aria-label={expanded.has(tx.id) ? 'Hide details' : 'Show details'}
                  onclick={() => toggle(tx.id)}
                >
                  <Icon name={expanded.has(tx.id) ? 'chevron-down' : 'chevron-right'} size={14} />
                </button>
              </td>
              <td>
                <span class="name mono truncate" title={tx.name}>{tx.name}</span>
                {#if tx.url && tx.url !== tx.name}
                  <span class="url muted truncate" title={tx.url}>{tx.url}</span>
                {/if}
              </td>
              <td><Badge tone="neutral" size="sm">{tx.op.replace('_', ' ')}</Badge></td>
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
                  <!--
                    Straight to the session timeline, where this span sits
                    beside the events and errors around it — the question
                    somebody asks the moment they see a slow call.
                  -->
                  <a
                    class="session mono truncate"
                    href={`#/sessions/${encodeURIComponent(tx.session_id)}`}
                    title={tx.session_id}
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
                     otherwise suppress every line break inside the panel. -->
                <td class="wrap" colspan="8">
                  <TransactionDetailPanel transaction={tx} />
                </td>
              </tr>
            {/if}
          {/each}
        {/snippet}
      </DataTable>
      <CursorPagination
        {total}
        totalIsCapped={totalCapped}
        page={pageNumber(list.page)}
        limit={LIMIT}
        canNext={nextCursor !== null}
        busy={loading}
        noun="transaction"
        {onjump}
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
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  .center {
    display: flex;
    justify-content: center;
    padding: 40px 0;
  }
  .stale-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    padding: 8px 14px;
    font-size: 12.5px;
    background: var(--warning-soft);
    color: var(--warning);
  }
  .name {
    display: block;
    max-width: 380px;
  }
  .url {
    display: block;
    max-width: 380px;
    font-size: 11.5px;
  }
  /* Fixed width so the chevron column does not resize between a page whose
     rows all carry metadata and one where none do. */
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
    background: var(--surface-2);
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
</style>
