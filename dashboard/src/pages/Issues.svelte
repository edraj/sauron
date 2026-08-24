<script lang="ts">
  import { t, joinList } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import { rowHref, rowNav } from '../lib/utils/row-link';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import CursorPagination from '../lib/components/CursorPagination.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import {
    fromParams,
    rangeKey,
    toParams,
    toPredicate,
    type DateRangeValue,
  } from '../lib/models/date-range';
  import {
    ISSUE_FIELDS,
    encodeFilters,
    gatedFilterFields,
    parseFilters,
    type Filter,
  } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listIssues, getIssueStats } from '../lib/api/issues';
  import type { SearchEnvelope } from '../lib/api/search';
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';
  import { preflight, queryErrorFor } from '../lib/utils/query-error';
  import { compactNumber } from '../lib/utils/format';
  import {
    canGoBack,
    cursorOf,
    emptyPage,
    goToPage,
    offsetOf,
    pageKey,
    pageNumber,
    type CursorPage,
  } from '../lib/models/cursor-page';
  import { panelScopeNote } from '../lib/models/panel-scope';
  import type { Issue, IssueStats } from '../lib/models';

  // Issues defaults to "All" time (open issues shouldn't drop off the landing
  // view just because they're old); the picker narrows on demand. 3650d is the
  // backend's effective-all cap for the issues list.
  /**
   * Rows per page. Was an inline `limit: 100` at the one request; named now
   * because the pager also needs it — a numbered jump turns a page number into
   * an offset, and doing that with a different number than the request uses
   * lands on the wrong rows.
   */
  const ISSUES_LIMIT = 100;

  // The day counts are the identity and never move; only the widest one's
  // label is a word rather than a unit abbreviation, so only the label is
  // translated. Kept as a plain const so `WIDEST_RANGE` below can read it at
  // init — a `$derived` array would make that read capture a stale first value.
  const ISSUE_RANGE_DAYS = [7, 30, 90, 3650];
  const WIDEST_RANGE = Math.max(...ISSUE_RANGE_DAYS);
  const ISSUE_RANGES = $derived(
    ISSUE_RANGE_DAYS.map((days) => ({
      days,
      label: days === WIDEST_RANGE ? t('ui.range.all') : `${days}d`,
    })),
  );

  // `WIDEST_RANGE` is the widest setting the picker offers, read off the list
  // above rather than written out again: at that setting the range narrows
  // nothing the reader can perceive, so it cannot be what makes a panel and the
  // list disagree. Derived so that adding a range can't leave a hardcoded 3650
  // behind.

  // Hydrate filter/search/date-range state from the URL once, at init — not
  // inside an effect, so this never re-runs and never fights the sync effect
  // below.
  const initial = new URLSearchParams($querystring ?? '');
  const parsedFilters = parseFilters(initial.getAll('filter'), ISSUE_FIELDS);
  // Default view: unresolved-only, but ONLY when the URL carried no `filter`
  // at all (so an explicit empty-filter URL, e.g. "show everything", sticks).
  const initialFilters: Filter[] =
    parsedFilters.length === 0 && !initial.has('filter')
      ? [{ field: 'status', op: 'eq', value: 'unresolved' }]
      : parsedFilters;
  let filters = $state<Filter[]>(initialFilters);
  let search = $state(initial.get('q') ?? '');
  // The URL-sync/reload effect below depends on this, not on `search` directly,
  // so free-text typing doesn't fire a backend request + history.replaceState
  // on every keystroke. Filters and the date range still apply immediately.
  let appliedSearch = $state(initial.get('q') ?? '');
  // The URL wins over the shared selection when it carries one: a link is an
  // explicit request for a specific window, and honouring the store instead
  // would make a shared link open on someone else's range. Otherwise the
  // shared selection applies, falling back to this page's own widest setting.
  let range = $state<DateRangeValue>(
    initial.get('since_days') || initial.get('from')
      ? fromParams(initial, WIDEST_RANGE)
      : rangeStore.effective(WIDEST_RANGE),
  );

  // Two cached views: the issue list and the stat tiles. Each owns its own
  // data/loading/revalidating/error state and the stale-while-revalidate policy
  // (lib/stores/cached-view.svelte.ts); this page only supplies the cache key and
  // the fetcher.
  //
  // Re-exposed under the names the template already used, so the markup did not
  // change: `loading` still means "nothing to show" and now `revalidating` means
  // "cached rows are up, refreshing behind them".
  //
  // The list view caches the whole `SearchEnvelope`, not just its rows: `total`
  // and `next_cursor` describe the very payload being cached, so splitting them
  // out would leave a cache hit able to repaint rows with no idea whether more
  // follow them.
  const issuesView = new CachedView<SearchEnvelope<Issue>>();
  const statsView = new CachedView<IssueStats>();

  const issues = $derived(issuesView.data?.data ?? []);
  const loading = $derived(issuesView.loading);
  const revalidating = $derived(issuesView.revalidating);
  const error = $derived(issuesView.error);

  /**
   * `error` is two different situations and they need two different screens.
   *
   * `CachedView` deliberately KEEPS the rows when a **forced** Refresh or Retry
   * fails over data it already had — blanking a populated table over one bad
   * poll is worse than showing data a minute old — and sets `error` alongside
   * them so the failure is not swallowed. Rendering the error card on `error`
   * alone throws those retained rows away, which is the one outcome the store
   * went out of its way to prevent. It also loses the reader's place in the
   * walk: `showPager` gated on `!error`, so the pager left with the table and
   * page 7 of a cursor walk is not somewhere you can navigate back to.
   *
   * So: `fatalError` is "there is nothing to show" and owns the card;
   * `staleError` is "these rows are older than you asked for" and is a line
   * above them.
   */
  const fatalError = $derived(error !== null && !issuesView.hasData);
  const staleError = $derived(error !== null && issuesView.hasData);

  /**
   * Which page of the keyset walk is on screen.
   *
   * `$state.raw` because the reducer replaces the object wholesale and never
   * edits it in place, so the deep proxy would be pure overhead.
   *
   * Moved by a click, or reset by the predicate effect below — never by a
   * response. `models/cursor-page.ts` explains why that separation is the
   * whole design, and what breaks without it (a Refresh on page 2 silently
   * stepping the state to page 3).
   */
  let page = $state.raw<CursorPage>(emptyPage());

  /**
   * The cursor for the NEXT page, read off the envelope that produced the rows
   * currently rendered — so the Next button's enabled state and the cursor that
   * button sends come from one payload and cannot disagree.
   */
  const nextCursor = $derived(issuesView.data?.next_cursor ?? null);
  /**
   * `null`, not `0`, when no envelope is on screen.
   *
   * `CachedView.load` clears `data` on a cache MISS, and every first visit to a
   * page of the walk is a miss because the cursor is in the key. A `?? 0` here
   * would have the pager state "No issues · Page 3" for the length of every
   * such request; `null` makes it state the page number and no count at all.
   */
  const total = $derived(issuesView.data?.total ?? null);
  const totalIsCapped = $derived(issuesView.data?.total_is_capped ?? false);

  /**
   * The planner narrowed the time window, and by how much.
   *
   * `resolve_window` computes this as the TIGHTEST of the caller's own window,
   * the route's ceiling and the cost clamp, naming the rule that actually
   * bound — and until `SearchDisclosure` nothing on this page read it, so a
   * query served 30 days of a 365-day request looked like a complete answer.
   */
  const clamped = $derived(issuesView.data?.clamped ?? null);

  /**
   * The resource's own schema, held for `did you mean`. A failure is silent:
   * without it the server's 400 still shows, just with no suggestion attached.
   */
  let searchSchema = $state<SchemaDefinition | null>(null);
  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    fetchSchema(id, 'issues')
      .then((s) => {
        if (!cancelled) searchSchema = s;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  /**
   * What to mark ON the input, as opposed to on the page's error card.
   *
   * A local parse problem wins: it is more specific than whatever the last
   * request said, and it means no request was worth issuing at all. Anything
   * that is not a 400 or a 403 stays the card's business — a 500 is not a
   * query the reader can fix by editing it.
   */
  const searchError = $derived(
    preflight(search) ?? queryErrorFor(issuesView.errorStatus, error, searchSchema),
  );

  /**
   * True once the reader has moved off page one, and until the predicate effect
   * below resets the walk.
   *
   * "Is a walk in progress?" cannot be read off the rows or off `page` during a
   * move. `CachedView` drops `data` on a cache miss, so `issues.length` is 0
   * while the next page is in flight; and a Prev landing back on page one has
   * `canGoBack(page)` false as well. A pager keyed on either unmounts for the
   * length of the move and remounts when it lands — the card collapsing to a
   * spinner, the control jumping out from under the cursor, and (because a
   * cached Prev repaints instantly) doing it in one direction only.
   */
  let walked = $state(false);

  /**
   * Shown whenever there are rows to page through, or the reader has walked off
   * page one — including while a page move, a Refresh or a background
   * revalidate is in flight over it, which is why this does not test `loading`
   * at all. The control stays put and `busy` disables the buttons instead,
   * while `total` goes `null` so the label states a page number rather than the
   * zero it would otherwise read off an absent envelope.
   *
   * This also covers a Next that lands on an empty page: the server should
   * never issue a cursor for one, but if it ever did, hiding the pager along
   * with the table would leave no control back.
   */
  const showPager = $derived(!fatalError && (issues.length > 0 || walked));

  /**
   * A landed page with no rows on it, which is not the same fact as "nothing
   * matches" and must not borrow its copy — `total` is a fresh count of the
   * whole match set while the cursor is a boundary from an earlier request, so
   * a retention trim or a deletion between two clicks lands here with `412
   * issues` in the caption above an empty table.
   */
  const emptyPastFirstPage = $derived(
    !loading && !fatalError && issues.length === 0 && canGoBack(page),
  );

  /**
   * A 403 while a permission-gated filter is applied: the API refuses to answer
   * a predicate over a column this role may not read.
   *
   * Retrying cannot fix it — the grant will not change between clicks — so the
   * page offers to drop the chip instead. Requires BOTH conditions: a 403 with
   * no gated filter applied is a genuine loss of access to the page, and
   * suggesting "remove the filter" there would send the user chasing a chip that
   * is not the cause.
   */
  const blockedFilterFields = $derived(
    issuesView.errorStatus === 403 ? gatedFilterFields(filters) : [],
  );

  /** "Tag", or "Tag and Workflow" — the chip labels, not the wire keys. */
  const blockedFilterLabels = $derived(
    joinList(
      blockedFilterFields.map((k) => {
        const def = ISSUE_FIELDS.find((f) => f.key === k);
        return def ? t(def.labelKey) : k;
      }),
    ),
  );

  function dropBlockedFilters() {
    const blocked = new Set(blockedFilterFields);
    filters = filters.filter((f) => !blocked.has(f.field));
  }

  const stats = $derived(statsView.data ?? null);
  const loadingStats = $derived(statsView.loading);
  const revalidatingStats = $derived(statsView.revalidating);

  /**
   * What the header panels leave out of the query the list below them runs.
   *
   * Both are fetched by `loadStats`, which calls
   * `GET /v1/apps/{id}/issues/stats` with `since_days` and nothing else — no
   * `filter`, no `q` — so neither reads the FilterBar at all. That is
   * deliberate and is not a bug to fix here: with the default
   * `status:unresolved` chip applied, a filtered `Unresolved` tile would equal
   * `Total` and the other five would read 0. They are the broad view. These
   * captions are the part that was missing, which is any acknowledgement that
   * they are a different set from the rows.
   *
   * `appliedSearch`, not `search`: the list runs the SUBMITTED value, so
   * keying off the raw box would post the caption mid-keystroke, while the
   * rows on screen still match the tiles and there is nothing to disclose yet.
   *
   * See `models/panel-scope.ts` for why these sentences name what the panel
   * dropped rather than the scope it covers ("app-wide" would have been a lie:
   * every one of these routes is environment-scoped too).
   */
  const tilesNote = $derived(
    panelScopeNote(
      {
        ignoredFilters: filters.length,
        ignoresSearch: appliedSearch !== '',
        // The tiles ignore the range even though the request sends it:
        // `repo::issue_stats` builds a `count(*) ... WHERE app_id=$1` with no
        // date predicate at all, so `since_days` reaches the handler and is
        // used only for the series below. Counted as ignored at every setting
        // but the widest, where it narrows nothing anyway.
        ignoresDateRange: range.kind === 'absolute' || range.days < WIDEST_RANGE,
      },
      'totals',
    ),
  );

  /**
   * The Occurrences chart is the `series` half of that same payload, and it
   * DOES honour `since_days` (`repo::error_series` takes the cutoff) — so its
   * caption must not claim the range was dropped. It is the predicate, and
   * only the predicate, that this chart does not carry.
   */
  const occurrencesNote = $derived(
    panelScopeNote(
      {
        ignoredFilters: filters.length,
        ignoresSearch: appliedSearch !== '',
        ignoresDateRange: false,
      },
      'chart',
    ),
  );

  let refreshing = $state(false);

  const isUnresolvedDefault = $derived(
    !search &&
      filters.length === 1 &&
      filters[0].field === 'status' &&
      filters[0].op === 'eq' &&
      filters[0].value === 'unresolved',
  );

  /**
   * `force` bypasses the fresh-window short-circuit: the Refresh button and the
   * error-state Retry both mean "go to the network now".
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's issues would be served as another's.
   *
   * The CURSOR is in the key for a blunter reason: without it every page of a
   * walk shares one entry, so the first Next click inside the fresh window is
   * served page one straight back out of the cache with no request on the wire
   * at all — a pager that looks inert. `viewKey` keeps `undefined` as a distinct
   * token, so page one's key is its own entry rather than a prefix of the others.
   *
   * Each page therefore becomes its own entry, which is what makes Prev repaint
   * instantly. The `issues.list` prefix that `IssueDetail` invalidates after a
   * status change still clears all of them: `ViewCache.invalidate` matches on
   * the raw key string, and every one of these keys starts with the view name.
   */
  async function load(appId: string, q: string, p: CursorPage, force = false) {
    const enc = encodeFilters(filters);
    const cursor = cursorOf(p);
    const offset = offsetOf(p);
    await issuesView.load(
      // `pageKey`, NOT `cursor`: a page reached by a numbered jump carries a
      // null cursor, which is what page 1 carries — keyed on the cursor alone,
      // page 7 would hash to page 1's entry and repaint the first page out of
      // the cache with no request on the wire to notice.
      viewKey('issues.list', appId, sessionStore.scopeKey, enc, q, rangeKey(range), pageKey(p)),
      () =>
        listIssues(appId, {
          filters: enc,
          query: q || undefined,
          ...toPredicate(range),
          limit: ISSUES_LIMIT,
          cursor,
          offset,
        }),
      force,
    );
  }

  /**
   * Prev/Next load IMPERATIVELY rather than by writing state the reload effect
   * reads back. An effect that both wrote the page and read it to build its
   * request would re-run on its own write; this way the effect depends only on
   * the predicate inputs, and paging never enters it.
   */
  function toPage(p: CursorPage) {
    const aid = sessionStore.currentAppId;
    // The walk does not move unless the request can actually be issued. Written
    // the other way round — state first, request only `if (aid)` — a click with
    // no app selected leaves the pager reading "Page 2" with nothing in flight
    // and no way out but a filter change. `AppShell requireApp` makes that
    // unreachable in practice, so this guards a shape rather than a live bug.
    if (!aid) return;
    page = p;
    walked = true;
    void load(aid, appliedSearch, p);
  }

  /**
   * Move to a numbered page.
   *
   * `goToPage` refuses any move it cannot make — the target already on screen,
   * no next cursor to step with, a page below 1 — and says so by handing back
   * the very object it was given. Testing identity keeps every one of those
   * rules in the reducer, and skips the reload rather than refetching the page
   * already on screen.
   */
  function onjump(target: number) {
    const next = goToPage(page, target, nextCursor, ISSUES_LIMIT);
    if (next !== page) toPage(next);
  }

  /** The empty-state's "Back a page" escape hatch, in terms of the same reducer. */
  function goPrev() {
    onjump(pageNumber(page) - 1);
  }

  async function loadStats(appId: string, win: DateRangeValue, force = false) {
    await statsView.load(
      viewKey('issues.stats', appId, sessionStore.scopeKey, rangeKey(win)),
      () => getIssueStats(appId, win),
      force,
    );
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      // force: an explicit click must always reach the network, cache or not.
      // `page` unchanged: Refresh means "this page again, current data", and a
      // refresh that also moved you off the rows you were reading would be a
      // different control.
      await Promise.all([load(aid, appliedSearch, page, true), loadStats(aid, range, true)]);
    } finally {
      refreshing = false;
    }
  }

  // The search box applies on submit only (button/Enter/clear). Filters and
  // the date range still reload immediately; a query, unlike a chip, spends
  // most of its typing life as an invalid fragment.
  function onSearch(q: string) {
    appliedSearch = q;
  }

  // Re-query + rewrite the URL whenever filter/appliedSearch/date-range state
  // changes. Depends on `appliedSearch` (submitted), not `search`, so this
  // doesn't fire per keystroke.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const enc = encodeFilters(filters);
    const s = appliedSearch;
    const win = range;
    if (!aid) return;
    const p = new URLSearchParams();
    for (const f of enc) p.append('filter', f);
    if (s) p.set('q', s);
    // `toParams` decides between `since_days` and `from`/`to`, so a shared URL
    // cannot carry both — the same encoder the request uses.
    for (const [k, v] of Object.entries(toParams(win))) p.set(k, v);
    void replace(`/issues?${p.toString()}`);
    // Back to page one. A cursor addresses a position in ONE result set, so it
    // is meaningless against a different predicate — and equally meaningless
    // against a different environment, which is why touching `scopeKey` above
    // has to reset this too and not merely refetch.
    //
    // Written but never READ here, and the load takes the fresh page as an
    // argument rather than reading the state back: an effect that read `page`
    // would re-run on its own write, which is how this project's last
    // self-defeating reset effect looped.
    const first = emptyPage();
    page = first;
    // The walk is over, so the pager goes with it: page one of a new predicate
    // gets the plain spinner it had before any of this, not a pager hovering
    // above it.
    walked = false;
    void load(aid, s, first);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const win = range;
    if (aid) {
      void loadStats(aid, win);
    }
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">{t('issues.title')}</h1>
      <p class="muted sub">{t('issues.subtitle')}</p>
    </div>
    <div class="controls">
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached data, fetching fresh" hint, and without it
        the instant paint is indistinguishable from live data.
      -->
      <RefreshButton
        onclick={refresh}
        loading={refreshing || revalidating || revalidatingStats}
        title={revalidating || revalidatingStats ? 'Refreshing…' : 'Refresh'}
      />
    </div>
  </div>

  {#if stats}
    <StatTiles min={140}>
      <StatTile label={t('issues.stat.total')} value={compactNumber(stats.total)} />
      <StatTile label={t('issues.stat.unresolved')} value={compactNumber(stats.unresolved)} tone="warning" />
      <StatTile label={t('issues.stat.resolved')} value={compactNumber(stats.resolved)} tone="success" />
      <StatTile label={t('issues.stat.ignored')} value={compactNumber(stats.ignored)} tone="neutral" />
      <StatTile label={t('issues.stat.fatal')} value={compactNumber(stats.fatal)} tone="error" />
      <StatTile label={t('issues.stat.error')} value={compactNumber(stats.error)} tone="error" />
      <StatTile label={t('issues.stat.warning')} value={compactNumber(stats.warning)} tone="warning" />
    </StatTiles>

    <!--
      Always rendered, and empty most of the time. The line reserves its own
      height whether or not it has anything to say, so a chip added or removed
      swaps the text in place instead of shunting the chart and the whole table
      down by a line — see the `min-height` on `.scope-note`.
    -->
    <p class="scope-note">{tilesNote ?? ''}</p>

    <div class="occ">
      <Card title={t('issues.occurrences')}>
        <TimeSeriesChart data={stats.series} height={200} color="var(--error)" />
        <p class="scope-note in-card">{occurrencesNote ?? ''}</p>
      </Card>
    </div>
  {:else if loadingStats}
    <div class="center-sm"><Spinner size={22} /></div>
  {/if}

  <FilterBar fields={ISSUE_FIELDS} bind:filters bind:search bind:range ranges={ISSUE_RANGES} appId={sessionStore.currentAppId ?? undefined} context="issues" error={searchError} {onSearch} />
  <SearchDisclosure {clamped} />

  <Card padding="none">
    <!--
      A forced Refresh or Retry that failed over rows we already had. The rows
      below are real, just older than the reader asked for, so they stay — and
      so does the pager, which is the only way back to page 7 of a walk.
      `role="status"` rather than `alert`: nothing is broken on screen and
      nothing needs interrupting, the data is simply not as fresh as requested.
    -->
    {#if staleError}
      <p class="stale-banner" role="status">
        <Icon name="triangle-alert" size={14} />
        <span>Showing the last results that loaded — refreshing failed: {error}</span>
        <Button
          variant="ghost"
          size="sm"
          onclick={() =>
            sessionStore.currentAppId &&
            load(sessionStore.currentAppId, appliedSearch, page, true)}
        >
          {t('ui.tryAgain')}
        </Button>
      </p>
    {/if}
    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if fatalError}
      <EmptyState
        title={blockedFilterFields.length > 0
          ? 'This filter needs more access'
          : "Couldn't load issues"}
        description={error ?? undefined}
        icon="triangle-alert"
      >
        {#snippet action()}
          {#if blockedFilterFields.length > 0}
            <!-- Not a Retry: the grant will not change between clicks, so the
                 only route back to a rendered page is dropping the chip. -->
            <Button variant="secondary" onclick={dropBlockedFilters}>
              Remove {blockedFilterLabels} filter{blockedFilterFields.length > 1 ? 's' : ''}
            </Button>
          {:else}
            <Button
              variant="secondary"
              onclick={() =>
                sessionStore.currentAppId &&
                load(sessionStore.currentAppId, appliedSearch, page, true)}
            >
              {t('common.retry')}
            </Button>
          {/if}
        {/snippet}
      </EmptyState>
    {:else if emptyPastFirstPage}
      <!--
        Deliberately not the copy below. That one answers "does anything match?"
        and the answer here is yes — `total` says so in the pager underneath.
        What happened is that this page of the walk no longer holds any of them,
        so "Your app is behaving" over a caption reading "412 issues · Page 2"
        would be the pager lying in prose.
      -->
      <EmptyState
        title={t('list.stale.title')}
        description={t('list.stale.issuesBody')}
        icon="search"
      >
        {#snippet action()}
          <Button variant="secondary" onclick={goPrev} disabled={loading || revalidating}>
            {t('list.stale.backAPage')}
          </Button>
        {/snippet}
      </EmptyState>
    {:else if issues.length === 0}
      <EmptyState
        title={t('issues.empty.title')}
        description={isUnresolvedDefault
          ? 'Nothing unresolved right now. Your app is behaving.'
          : 'No issues match these filters.'}
        icon="check"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th class="col-title">{t('issues.column.issue')}</th>
            <th>{t('issues.column.level')}</th>
            <th>{t('common.status')}</th>
            <th class="num">{t('overview.stat.events')}</th>
            <th class="num">{t('overview.stat.users')}</th>
            <th>{t('issues.column.lastSeen')}</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each issues as issue (issue.id)}
            {@const path = `/issues/${issue.id}`}
            <tr
              class="clickable"
              onclick={(e) => rowNav(e, path)}
              onauxclick={(e) => rowNav(e, path)}
            >
              <td class="col-title">
                <a class="row-link title-cell" href={rowHref(path)}>
                  <span class="issue-title">{issue.title}</span>
                  <span class="issue-sub mono">
                    {issue.type}{issue.culprit ? ` · ${issue.culprit}` : ''}
                  </span>
                </a>
              </td>
              <td><LevelBadge level={issue.level} size="sm" /></td>
              <td><StatusBadge status={issue.status} size="sm" /></td>
              <td class="num">{formatNumber(issue.times_seen)}</td>
              <td class="num">{formatNumber(issue.users_seen)}</td>
              <td><TimeValue value={issue.last_seen} muted /></td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    {/if}

    <!--
      `busy` takes `loading` as well as `revalidating`: a page move is a cache
      miss, and a miss is `loading`. That is the window the buttons most need to
      be dead in — a second click during it would walk off the `next_cursor` of
      the page being left.
    -->
    {#if showPager}
      <CursorPagination
        {total}
        {totalIsCapped}
        page={pageNumber(page)}
        limit={ISSUES_LIMIT}
        canNext={nextCursor !== null}
        busy={loading || revalidating}
        noun="issue"
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
    margin-bottom: 18px;
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
  /* Disclosure line under a header panel. `min-height` is the whole trick:
     the element is in the flow even when `panelScopeNote` returns null, so the
     caption appearing is a text swap and never a reflow. One line is enough —
     the longest sentence the model can build is ~60 characters, which fits a
     tile row at every width the grid wraps to. */
  .scope-note {
    font-size: 12px;
    line-height: 16px;
    min-height: 16px;
    color: var(--text-faint);
    margin: 8px 0 0;
  }
  /* Sits INSIDE the card, above the rows it qualifies, so the sentence and the
     stale data it describes cannot be read separately. `padding: none` on the
     Card means this supplies its own. */
  .stale-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    padding: 10px 14px;
    font-size: 12.5px;
    line-height: 18px;
    color: var(--text-muted);
    background: var(--warning-soft);
    border-bottom: 1px solid var(--border);
  }
  .stale-banner :global(svg) {
    color: var(--warning);
    flex: none;
  }
  .stale-banner span {
    flex: 1;
    min-width: 0;
  }
  /* Inside a Card the line sits under the panel's own content, so it needs the
     gap above it that the page-level one gets from its margin. */
  .scope-note.in-card {
    margin-top: 10px;
  }
  /* Was 14px. The reserved line above already contributes the gap the tiles
     used to get from this margin alone. */
  .occ {
    margin: 6px 0 18px;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 60px;
  }
  .center-sm {
    display: grid;
    place-items: center;
    padding: 32px;
    margin-bottom: 18px;
  }
  .col-title {
    min-width: 280px;
  }
  .title-cell {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    max-width: 480px;
  }
  .issue-title {
    font-weight: 560;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .issue-sub {
    font-size: 11.5px;
    color: var(--text-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
