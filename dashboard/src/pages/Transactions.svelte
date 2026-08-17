<script lang="ts">
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
  import JsonTree from '../lib/components/JsonTree.svelte';
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

  interface DetailRow {
    label: string;
    /** `null` renders an em dash — the field is genuinely absent on this span. */
    value: string | null;
    href?: string;
    mono?: boolean;
  }

  /**
   * Every stored field of a span, as label/value pairs for the detail panel.
   *
   * Deliberately hand-written rather than iterating the object's keys. A
   * key-walk would render `id`, `app_id` and `restored_pin_id` beside `url`
   * with equal weight, invent labels from column names, and — the part that
   * matters — silently start displaying whatever column the table grows next,
   * including one nobody decided was safe to show. This list is the decision.
   *
   * `ip_address` is omitted on purpose: the API already masks it
   * (`serialize_masked_ip`) and nulls it for a caller without `event:read`, so
   * the value here is at best a truncated address and at worst a blank field
   * that reads as "no IP recorded".
   */
  function detailRows(t: Transaction): DetailRow[] {
    return [
      { label: 'Name', value: t.name, mono: true },
      { label: 'Operation', value: t.op },
      { label: 'Duration', value: `${t.duration_ms.toLocaleString()} ms` },
      { label: 'Status', value: t.status },
      { label: 'HTTP method', value: t.http_method },
      { label: 'HTTP status', value: t.http_status == null ? null : String(t.http_status) },
      { label: 'URL', value: t.url, mono: true },
      {
        label: 'User',
        value: t.distinct_id,
        href: t.distinct_id ? `#/persons/${encodeURIComponent(t.distinct_id)}` : undefined,
        mono: true,
      },
      {
        label: 'Session',
        value: t.session_id,
        href: t.session_id ? `#/sessions/${encodeURIComponent(t.session_id)}` : undefined,
        mono: true,
      },
      {
        label: 'Device',
        value: t.device_key,
        href: t.device_key ? `#/devices/${encodeURIComponent(t.device_key)}` : undefined,
        mono: true,
      },
      { label: 'Release', value: t.release, mono: true },
      { label: 'Workflow', value: t.workflow_name },
      { label: 'Occurred at', value: t.occurred_at, mono: true },
      // Both timestamps, always. The GAP between them is the interesting
      // number on a mobile SDK — a span that occurred hours before it arrived
      // came out of an offline queue, or off a device with a skewed clock, and
      // either fact changes how you read the one above.
      { label: 'Received at', value: t.received_at, mono: true },
      { label: 'Finished at', value: t.finished_at, mono: true },
      { label: 'Transaction id', value: t.id, mono: true },
    ];
  }

  /** The SDK capped this payload — the span is real, the blob is a marker. */
  function isTruncated(t: Transaction): boolean {
    return t.extra?._truncated === true;
  }

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
      <h1>Transactions</h1>
      <p class="sub muted">
        Individual timed operations. The
        <a href="#/performance">Performance</a> page aggregates these by operation.
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

  <Card padding="none" title="Spans">
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
        <Button variant="ghost" size="sm" onclick={refresh}>Try again</Button>
      </p>
    {/if}

    {#if loading && rows.length === 0}
      <div class="center"><Spinner size={22} /></div>
    {:else if fatalError}
      <EmptyState title="Couldn't load transactions" description={error ?? undefined} icon="triangle-alert">
        {#snippet action()}
          <Button onclick={refresh}>Try again</Button>
        {/snippet}
      </EmptyState>
    {:else if rows.length === 0}
      <EmptyState
        title="No transactions"
        description="Nothing matched this query in the selected window. Record one with trackTransaction() in any Sauron SDK."
        icon="timer"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th class="chev" aria-label="Expand"></th>
            <SortableTh key="name" columnDefault="asc" sort={list.sort} {onsort}>Name</SortableTh>
            <SortableTh key="op" columnDefault="asc" sort={list.sort} {onsort}>Op</SortableTh>
            <SortableTh key="duration_ms" class="num" sort={list.sort} {onsort}>Duration</SortableTh>
            <th>Status</th>
            <th>HTTP</th>
            <!--
              Not a `SortableTh`. The backend's sort whitelist is
              occurred_at/duration_ms/name/op — the four orderings with a
              keyset index behind them — and asking for anything else is a 400
              that names the allowed set. A header that looks clickable and
              answers with an error is worse than one that plainly does not
              sort. Narrow instead: click the id, or filter `session:<id>`.
            -->
            <th>Session</th>
            <SortableTh key="occurred_at" sort={list.sort} {onsort}>When</SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as t (t.id)}
            <tr class:expanded={expanded.has(t.id)}>
              <td class="chev">
                <!--
                  Unconditional. It used to appear only on rows carrying
                  tags/extra, back when expanding showed nothing else — now it
                  opens the full span, which every row has.
                -->
                <button
                  class="chev-btn"
                  aria-expanded={expanded.has(t.id)}
                  aria-label={expanded.has(t.id) ? 'Hide details' : 'Show details'}
                  onclick={() => toggle(t.id)}
                >
                  <Icon name={expanded.has(t.id) ? 'chevron-down' : 'chevron-right'} size={14} />
                </button>
              </td>
              <td>
                <span class="name mono truncate" title={t.name}>{t.name}</span>
                {#if t.url && t.url !== t.name}
                  <span class="url muted truncate" title={t.url}>{t.url}</span>
                {/if}
              </td>
              <td><Badge tone="neutral" size="sm">{t.op.replace('_', ' ')}</Badge></td>
              <td class="num"><LatencyBadge ms={t.duration_ms} size="sm" /></td>
              <td>
                {#if t.status}
                  <Badge tone={t.status === 'ok' ? 'success' : 'warning'} size="sm">{t.status}</Badge>
                {:else}
                  <span class="muted">—</span>
                {/if}
              </td>
              <td>
                {#if t.http_status != null}
                  <Badge tone={httpStatusTone(t.http_status)} size="sm">
                    {t.http_method ?? ''}
                    {t.http_status}
                  </Badge>
                {:else if t.http_method}
                  <Badge tone="neutral" size="sm">{t.http_method}</Badge>
                {:else}
                  <span class="muted">—</span>
                {/if}
              </td>
              <td>
                {#if t.session_id}
                  <!--
                    Straight to the session timeline, where this span sits
                    beside the events and errors around it — the question
                    somebody asks the moment they see a slow call.
                  -->
                  <a
                    class="session mono truncate"
                    href={`#/sessions/${encodeURIComponent(t.session_id)}`}
                    title={t.session_id}
                  >{t.session_id}</a>
                {:else}
                  <span class="muted" title="This span was recorded without a session">—</span>
                {/if}
              </td>
              <td><TimeValue value={t.occurred_at} /></td>
            </tr>
            {#if expanded.has(t.id)}
              <tr class="meta-row">
                <td colspan="8">
                  {#if isTruncated(t)}
                    <p class="truncated" role="status">
                      <Icon name="info" size={14} />
                      <span>
                        The SDK capped this payload at 16 KB and sent a marker instead
                        ({(t.extra?._bytes as number) < 0
                          ? 'the value could not be serialized'
                          : `${(t.extra?._bytes as number).toLocaleString()} bytes`}). The
                        span and its timing are accurate; only the attached data was dropped.
                      </span>
                    </p>
                  {/if}

                  <div class="meta-block">
                    <h4>Span</h4>
                    <dl class="detail">
                      {#each detailRows(t) as row (row.label)}
                        <div class="detail-row">
                          <dt>{row.label}</dt>
                          <dd>
                            {#if row.value === null}
                              <span class="muted">—</span>
                            {:else if row.href}
                              <a class="mono" href={row.href}>{row.value}</a>
                            {:else}
                              <span class:mono={row.mono}>{row.value}</span>
                            {/if}
                          </dd>
                        </div>
                      {/each}
                    </dl>
                  </div>

                  {#if t.tags === null}
                    <!--
                      `null` is WITHHELD, not empty — `strip_transaction_body`
                      nulls both for a caller without `event:read`. Saying so
                      beats rendering nothing, which reads as "this span had no
                      data" and sends people looking for a bug.
                    -->
                    <p class="withheld">
                      <Icon name="lock" size={13} />
                      <span>Tags and additional data are withheld — they need the <code>event:read</code> permission.</span>
                    </p>
                  {:else}
                    {#if Object.keys(t.tags).length > 0}
                      <div class="meta-block">
                        <h4>Tags</h4>
                        <div class="tag-list">
                          {#each Object.entries(t.tags) as [k, v] (k)}
                            <Badge tone="neutral" size="sm">{k}: {v}</Badge>
                          {/each}
                        </div>
                      </div>
                    {/if}
                    {#if t.extra && Object.keys(t.extra).length > 0}
                      <div class="meta-block">
                        <h4>Additional data</h4>
                        <JsonTree value={t.extra} expandTo={1} />
                      </div>
                    {/if}
                    {#if Object.keys(t.tags).length === 0 && (!t.extra || Object.keys(t.extra).length === 0)}
                      <p class="muted no-meta">
                        No tags or additional data on this span. Attach some by passing
                        <code>tags</code> / <code>extra</code> to <code>trackTransaction()</code>.
                      </p>
                    {/if}
                  {/if}
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
    padding-right: 0;
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
  .meta-block + .meta-block {
    margin-top: 14px;
  }
  .meta-block h4 {
    margin: 0 0 8px;
    font-size: 11.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
  }
  .tag-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  /* Two columns on a wide viewport, one when the pane is narrow. `auto-fill`
     rather than a fixed count so a maximised window does not stretch a
     16-row list into two very tall columns of mostly whitespace. */
  .detail {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: 2px 24px;
    margin: 0;
  }
  .detail-row {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 10px;
    align-items: baseline;
    padding: 3px 0;
    min-width: 0;
  }
  .detail dt {
    font-size: 12px;
    color: var(--muted);
  }
  .detail dd {
    margin: 0;
    font-size: 12.5px;
    /* Long urls and ids wrap instead of widening the grid track, which would
       otherwise push the whole table into a horizontal scroll. */
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .session {
    display: inline-block;
    max-width: 200px;
    vertical-align: bottom;
  }
  .withheld,
  .no-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 14px 0 0;
    font-size: 12.5px;
  }
  .withheld {
    color: var(--muted);
  }
  .truncated {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 14px;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: var(--info-soft);
    color: var(--info);
    font-size: 12.5px;
  }
</style>
