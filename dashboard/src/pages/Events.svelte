<script lang="ts">
  import { untrack } from 'svelte';
  import { querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import CursorPagination from '../lib/components/CursorPagination.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import TimeFilter from '../lib/components/TimeFilter.svelte';
  import {
    EVENT_FIELDS,
    encodeFilters,
    parseFilters,
    type Filter,
  } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { topEvents, eventSeries, listEvents } from '../lib/api/events';
  import type { SearchEnvelope } from '../lib/api/search';
  import { errorMessage, errorStatus } from '../lib/api/client';
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';
  import { preflight, queryErrorFor } from '../lib/utils/query-error';
  import {
    canGoBack,
    cursorOf,
    emptyPage,
    offsetOf,
    pageKey,
    pageNumber,
  } from '../lib/models/cursor-page';
  import {
    cursorGoTo,
    setCursorSort,
    type CursorListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import {
    fromParams,
    toParams,
    type TimeField,
    type TimeFilterState,
  } from '../lib/models/time-filter';
  import { panelScopeNote } from '../lib/models/panel-scope';
  import { viewKey } from '../lib/stores/view-cache';
  import type { AnalyticsEvent, SeriesPoint, TopEvent } from '../lib/models';

  const STREAM_LIMIT = 50;

  /** The chip label for `name`, so the caption below names what the bar shows. */
  const EVENT_NAME_LABEL = EVENT_FIELDS.find((f) => f.key === 'name')?.label ?? 'Event';

  // Hydrate filter/search/date-range state from the URL once, at init — not
  // inside an effect, so this never re-runs and never fights the sync effect
  // below.
  const initial = new URLSearchParams($querystring ?? '');
  let filters = $state<Filter[]>(parseFilters(initial.getAll('filter'), EVENT_FIELDS));
  let search = $state(initial.get('q') ?? '');
  // The URL-sync/reload and stream-load effects below depend on this, not on
  // `search` directly, so free-text typing doesn't fire a backend request +
  // history.replaceState on every keystroke. Filters and the date range still
  // apply immediately.
  let appliedSearch = $state(initial.get('q') ?? '');
  let sinceDays = $state(Number(initial.get('since_days')) || 30);

  /**
   * The stream's own window. ONE field, so the control renders a label rather
   * than a dropdown — `analytics_events` is `PARTITION BY RANGE (occurred_at)`
   * and `received_at` carries no index, so a `received_at` window would prune
   * no partitions and scan all of them. The single-entry whitelist makes
   * `?time_field=received_at` a 400 that names what is allowed, rather than a
   * parameter that looks accepted and is not.
   *
   * Governs the STREAM only. The two cards above keep `sinceDays` from the
   * FilterBar, because `/events/top` and `/events/series` take a plain day
   * count and cannot express an absolute bound.
   *
   * `stream_days` in the URL, not `since_days`: this page already spends
   * `since_days` on the cards' range, and one query string cannot carry two
   * different meanings for one name. The WIRE parameter is still `since_days`
   * — they are different endpoints, so there is no collision there — which is
   * what `streamWindowParams` below translates between.
   */
  const STREAM_TIME_FIELDS: TimeField[] = [{ key: 'occurred_at', label: 'Occurred' }];
  const STREAM_TIME_FIELD = 'occurred_at';

  /** URL shape (`stream_days`) -> model shape (`since_days`). */
  function readStreamWindow(sp: URLSearchParams): TimeFilterState {
    const shifted = new URLSearchParams(sp);
    const d = shifted.get('stream_days');
    shifted.delete('stream_days');
    shifted.delete('since_days');
    if (d) shifted.set('since_days', d);
    return fromParams(shifted, STREAM_TIME_FIELDS, STREAM_TIME_FIELD, 365);
  }

  /** Model shape -> URL shape. The inverse of `readStreamWindow`. */
  function writeStreamWindow(tf: TimeFilterState): URLSearchParams {
    const p = toParams(tf, STREAM_TIME_FIELD);
    const d = p.get('since_days');
    if (d !== null) {
      p.delete('since_days');
      p.set('stream_days', d);
    }
    return p;
  }

  let streamWindow = $state<TimeFilterState>(readStreamWindow(initial));

  const selectedTopEvent = $derived(
    filters.find((f) => f.field === 'name' && f.op === 'eq')?.value ?? null,
  );

  /**
   * What the two header cards leave out of the query the stream below them
   * runs. See `models/panel-scope.ts` for the shape and for why these
   * sentences name what was dropped instead of claiming a scope.
   *
   * Neither card reads the FilterBar: `topEvents` takes `since_days`/`limit`
   * and `eventSeries` takes `since_days`/`name`, so a `tag` chip or a search
   * that empties the stream leaves both of them showing the same app curve and
   * the same app-wide names as before. Passing the predicate through is not
   * the fix — the chart is useful precisely because it holds still while the
   * table moves — but saying nothing about it was, and that is what these two
   * lines are.
   *
   * `ignoresDateRange` is false on both: the picker's value is forwarded as
   * `since_days` to `topEvents` and `eventSeries` exactly as it is to the
   * stream, so the range is the one control all three panels agree on.
   *
   * `appliedSearch`, not `search`: the stream runs the SUBMITTED value, so
   * keying off the raw box would post the caption mid-keystroke while the rows
   * on screen still match the cards.
   */
  const volumeNote = $derived(
    panelScopeNote(
      {
        // Everything except the one chip `loadSeries` forwards as `name`.
        // Subtracting rather than re-filtering keeps this exact when a
        // hand-written URL carries two `name:eq` chips: `selectedTopEvent`
        // takes the first, and the second is genuinely ignored.
        ignoredFilters: filters.length - (selectedTopEvent !== null ? 1 : 0),
        ignoresSearch: appliedSearch !== '',
        ignoresDateRange: false,
        // The one place a chip does reach a header card. Without this the
        // chart would be captioned "the filters don't apply" while the event
        // name demonstrably does — which is the misreading the card's own
        // "Click an event to filter the chart and stream" hint sets up.
        appliedFilterLabel: selectedTopEvent !== null ? EVENT_NAME_LABEL : null,
      },
      'this chart',
    ),
  );

  /**
   * The top list drops the event-name chip as well — `topEvents` has no `name`
   * parameter — so its caption counts every chip, this one included. Clicking
   * a bar therefore captions the card it was clicked in, which is the point:
   * the selection highlights a row but does not re-rank or re-count the bars.
   */
  const topNote = $derived(
    panelScopeNote(
      {
        ignoredFilters: filters.length,
        ignoresSearch: appliedSearch !== '',
        ignoresDateRange: false,
      },
      'this list',
    ),
  );

  let top = $state<TopEvent[]>([]);
  let series = $state<SeriesPoint[]>([]);
  let loadingTop = $state(true);
  let loadingSeries = $state(true);
  let error = $state<string | null>(null);
  let refreshing = $state(false);

  // Raw event stream state.
  //
  // The whole `SearchEnvelope` is held rather than just its rows: `total` and
  // `next_cursor` describe this exact page, so keeping them together is what
  // stops a later refactor pairing one request's rows with another's count.
  //
  // `$state.raw`, not `$state`: the envelope is replaced wholesale on every
  // load and never edited in place, so the deep proxy would be pure overhead.
  //
  // There is no `streamOffset` any more. It used to drive a `<Pagination>`
  // below, and since S2c bridged this route onto keyset paging the server
  // accepts `offset` and ignores it — so clicking Next re-fetched page one
  // while the pager confidently relabelled those same 50 rows "51–100". A
  // control that silently lies is worse than no control, so it was removed and
  // `<CursorPagination>` now walks `next_cursor` instead.
  let streamPage = $state.raw<SearchEnvelope<AnalyticsEvent> | null>(null);
  const streamEvents = $derived(streamPage?.data ?? []);
  /**
   * `null`, not `0`, when no envelope is on screen: a page move clears
   * `streamPage` and the count only returns with the new page. A `?? 0` here
   * would have the pager state "No events · Page 3" for the length of every
   * move; `null` makes it state the page number and no count at all.
   */
  const streamTotal = $derived(streamPage?.total ?? null);
  const streamTotalCapped = $derived(streamPage?.total_is_capped ?? false);
  /**
   * The cursor for the next page, read off the envelope that produced the rows
   * on screen — so the Next button's enabled state and the cursor it sends come
   * from one payload and cannot disagree.
   */
  const streamNextCursor = $derived(streamPage?.next_cursor ?? null);
  let loadingStream = $state(true);
  let streamError = $state<string | null>(null);
  /**
   * The status behind `streamError`, so a rejected QUERY can be told apart
   * from a broken server. A 400 belongs on the search input; a 500 belongs on
   * the page's error card, and the two read much alike as prose.
   */
  let streamErrorStatus = $state<number | null>(null);

  /**
   * The planner narrowed the window. Events runs over the largest table in the
   * system and bounds its own request at 365 days on top of whatever the cost
   * clamp does, so this is the page most likely to serve less than was asked
   * for — and it said nothing until now.
   */
  const clamped = $derived(streamPage?.clamped ?? null);

  /** The resource's schema, held only for `did you mean`. */
  let searchSchema = $state<SchemaDefinition | null>(null);
  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    fetchSchema(id, 'events')
      .then((s) => {
        if (!cancelled) searchSchema = s;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  /** A local parse problem wins — it means no request was worth issuing. */
  const searchError = $derived(
    preflight(search) ?? queryErrorFor(streamErrorStatus, streamError, searchSchema),
  );
  /**
   * The inputs that produced `streamPage`, as a `viewKey` string.
   *
   * Only `loadStream`'s error path reads it, and only to answer one question:
   * did the request that just failed ask for the rows already on screen? That
   * is the difference between "your refresh failed, here is what you had" and
   * "these rows belong to the previous filter" — and it is not derivable from
   * the rows themselves.
   */
  let streamKey = $state<string | null>(null);
  let expandedId = $state<string | null>(null);

  /**
   * A failure with rows still on screen. See `loadStream`'s `catch`.
   */
  const staleStreamError = $derived(streamError !== null && streamEvents.length > 0);
  /** A failure with nothing left to show — this one owns the card. */
  const fatalStreamError = $derived(streamError !== null && streamEvents.length === 0);

  /**
   * Sort and page position for the raw stream, changed together — see
   * `models/list-state.ts` for why the two live in one field rather than as
   * independent `sort`/`page` variables.
   *
   * Moved by a click (`toPage`, `onsort`), or reset by the predicate effect
   * below — never by a response. See `models/cursor-page.ts` for why: fold
   * "record the server's next_cursor" into "move forward" and every Refresh
   * silently steps the page on while the rows stay put.
   */
  let list = $state<CursorListState>({
    sort: { key: 'occurred_at', dir: 'desc' },
    page: emptyPage(),
  });

  /**
   * True once the reader has moved off page one, and until the predicate effect
   * below resets the walk.
   *
   * "Is a walk in progress?" cannot be read off the rows or off `list.page`
   * during a move: `toPage` clears `streamPage`, so `streamEvents.length` is 0
   * while the request is in flight, and a Prev landing back on page one has
   * `canGoBack(list.page)` false as well. A pager keyed on either unmounts for
   * the length of the move and remounts when it lands — the card collapsing to
   * a spinner and the control jumping out from under the cursor.
   */
  let walked = $state(false);

  /**
   * Shown whenever there are rows to page through, or the reader has walked off
   * page one — including while a page move or a Refresh is in flight over it,
   * which is why this does not test `loadingStream` at all. The control stays
   * put and `busy` disables the buttons instead, while `streamTotal` goes
   * `null` so the label states a page number rather than the zero it would
   * otherwise read off a cleared envelope.
   *
   * This also covers a Next that lands on an empty page: without it the empty
   * state would hide the only control back.
   */
  const showPager = $derived(!fatalStreamError && (streamEvents.length > 0 || walked));

  /**
   * A landed page with no rows on it, which is not the same fact as "nothing
   * matches" and must not borrow its copy — `total` is a fresh count of the
   * whole match set while the cursor is a boundary from an earlier request, so
   * a retention trim or a deletion between two clicks lands here with `1,204+
   * events` in the caption above an empty table.
   */
  const emptyPastFirstPage = $derived(
    !loadingStream && !fatalStreamError && streamEvents.length === 0 && canGoBack(list.page),
  );

  async function loadTop(appId: string, days: number) {
    loadingTop = true;
    error = null;
    try {
      // Five, not twelve: the list sits beside the volume chart and the pair is
      // one row, so a long list is what made the two cards different heights.
      top = await topEvents(appId, { since_days: days, limit: 5 });
    } catch (err) {
      error = errorMessage(err);
      top = [];
    } finally {
      loadingTop = false;
    }
  }

  async function loadSeries(appId: string, days: number, name: string | null) {
    loadingSeries = true;
    try {
      series = await eventSeries(appId, {
        since_days: days,
        name: name ?? undefined,
      });
    } catch (err) {
      error = errorMessage(err);
      series = [];
    } finally {
      loadingSeries = false;
    }
  }

  /**
   * `streamGen` is an out-of-order guard: a filter change and a Prev/Next click
   * can have two requests in flight at once, and nothing about HTTP returns
   * them in order. Only the newest may write, so a slow response for a page the
   * user has already left cannot land under a pager that has moved on — which
   * would put one page's rows under another page's number.
   */
  let streamGen = 0;

  async function loadStream(
    appId: string,
    filterList: string[],
    q: string,
    tf: TimeFilterState,
    l: CursorListState,
  ) {
    const gen = ++streamGen;
    // Identifies the payload on screen, so a failure can tell "the refresh you
    // asked for failed" from "these rows answer a question you are no longer
    // asking". See the `catch`. Carries the sort too — a sort click reloads
    // through this same function, and `sortParam(l.sort)` is the half of the
    // key that changes on that click when the cursor does not (page one of a
    // new ordering is still page one, `cursorOf` alone would not tell them
    // apart).
    const key = viewKey(
      'events.stream',
      appId,
      filterList,
      q.trim(),
      // The window's DECLARATION, never the instant `last` resolves to: a
      // clock-derived key component mints a fresh entry on every load, so the
      // cache hits zero times while staying wired, typed and green.
      `${tf.field}:${tf.mode}:${tf.lastDays ?? ''}:${tf.from ?? ''}:${tf.to ?? ''}`,
      sortParam(l.sort),
      // `pageKey`, NOT `cursorOf`: a jumped page carries a null cursor, the
      // same as page 1, so a cursor-keyed entry would repaint page one.
      pageKey(l.page),
    );
    loadingStream = true;
    streamError = null;
    streamErrorStatus = null;
    try {
      const envelope = await listEvents(appId, {
        filters: filterList,
        q: q.trim() || undefined,
        // `sinceDays` is sent only in `last` mode; `predicateParams` drops it
        // whenever a bound is present, so the two can never both reach the wire.
        sinceDays: tf.mode === 'last' ? tf.lastDays : undefined,
        timeField: tf.field === STREAM_TIME_FIELD ? undefined : tf.field,
        from: tf.from,
        to: tf.to,
        limit: STREAM_LIMIT,
        sort: sortParam(l.sort),
        cursor: cursorOf(l.page),
        offset: offsetOf(l.page),
      });
      if (gen !== streamGen) return;
      streamPage = envelope;
      streamKey = key;
    } catch (err) {
      if (gen !== streamGen) return;
      streamError = errorMessage(err);
      streamErrorStatus = errorStatus(err);
      // Keep the rows ONLY when the request that failed was for the very inputs
      // that produced them — a Refresh or a Retry of what is already on screen.
      // Then the rows are still a true answer, merely older than asked for, and
      // the banner above them says so; blanking a populated table over one bad
      // poll loses the reader's place in the walk as well as the data.
      //
      // For any other key — a filter changed, the date range moved, a page move
      // — the rows on screen answer a DIFFERENT question, and leaving them under
      // the new chips would present them as the new result. That is the same
      // trap `CachedView.load` documents on its cache-miss branch.
      if (streamKey !== key) {
        streamPage = null;
        streamKey = null;
      }
    } finally {
      // Left to the newest call: a superseded one clearing this would drop the
      // spinner while its replacement is still in flight.
      if (gen === streamGen) loadingStream = false;
    }
  }

  /**
   * Prev/Next/sort load IMPERATIVELY rather than by writing state the reload
   * effect reads back. An effect that both wrote `list` and read it to build
   * its request would re-run on its own write; this way the effect depends
   * only on the predicate inputs, and neither paging nor sorting ever enters
   * it.
   */
  function toPage(next: CursorListState) {
    const aid = sessionStore.currentAppId;
    // Nothing moves unless the request can actually be issued. Written the other
    // way round — state first, request only `if (aid)` — a click with no app
    // selected clears the rows, steps the page number, and leaves "No events ·
    // Page 2" with Prev live, nothing in flight and no way out but a filter
    // change. `AppShell requireApp` makes that unreachable in practice, so this
    // guards a shape rather than a live bug.
    if (!aid) return;
    list = next;
    walked = true;
    // The rows up are the page being left. Clearing them is what stops the
    // pager labelling one page's rows with another's number while the request
    // is in flight — the table shows its spinner instead, as it does on a
    // filter change.
    streamPage = null;
    expandedId = null;
    void loadStream(aid, encodeFilters(filters), appliedSearch, streamWindow, next);
  }

  /**
   * Move to a numbered page.
   *
   * `cursorGoTo` picks the mechanism — a keyset step when the target is
   * adjacent and a cursor for it exists, an offset jump otherwise — and refuses
   * any move it cannot make by handing back the very `list` it was given.
   * Testing identity keeps every one of those rules in the reducer, and skips
   * the reload rather than refetching the page already on screen.
   */
  function onjump(target: number) {
    const next = cursorGoTo(list, target, streamNextCursor, STREAM_LIMIT);
    if (next !== list) toPage(next);
  }

  /** The empty-state's "Back a page" escape hatch, in terms of the same reducer. */
  function goPrev() {
    onjump(pageNumber(list.page) - 1);
  }

  /**
   * The sort-header click handler passed to every `SortableTh` in the stream
   * table. `setCursorSort` resets the walk onto the new ordering — a keyset
   * cursor only addresses a position within the ordering that minted it, so a
   * sort change cannot keep the old page — and this reloads directly for the
   * same reason `toPage` does: the predicate effect below must not depend on
   * `list` (see its comment), so nothing else will notice this write.
   */
  function onsort(key: string, columnDefault: SortDir) {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    const next = setCursorSort(list, key, columnDefault);
    list = next;
    walked = false;
    streamPage = null;
    expandedId = null;
    void loadStream(aid, encodeFilters(filters), appliedSearch, streamWindow, next);
  }

  // Re-fetch all page data with the current state (filters, search, date
  // range and pagination) left intact. Reuses the existing loaders.
  //
  // `page` goes in unchanged and the rows are NOT cleared: Refresh means "this
  // page again, current data", so it must neither move the walk nor blank the
  // table under the reader.
  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([
        loadTop(aid, sinceDays),
        loadSeries(aid, sinceDays, selectedTopEvent),
        loadStream(aid, encodeFilters(filters), appliedSearch, streamWindow, list),
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
    const days = sinceDays;
    if (aid) void loadTop(aid, days);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    const name = selectedTopEvent;
    if (aid) void loadSeries(aid, days, name);
  });

  // The search box applies on submit only (button/Enter/clear). Filters and
  // the date range still reload immediately; a query, unlike a chip, spends
  // most of its typing life as an invalid fragment.
  function onSearch(q: string) {
    appliedSearch = q;
  }

  // Rewrite the URL whenever filter/appliedSearch/date-range state changes, and
  // collapse any expanded row (its id belongs to the previous result set).
  // Depends on `appliedSearch` (submitted), not `search`, so this doesn't fire
  // per keystroke.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const enc = encodeFilters(filters);
    const s = appliedSearch;
    const days = sinceDays;
    const tf = streamWindow;
    if (!aid) return;
    const p = new URLSearchParams();
    for (const f of enc) p.append('filter', f);
    if (s) p.set('q', s);
    // The CARDS' range. The stream's own window is appended below under
    // `stream_days`/`from`/`to`, which is why these two can coexist here.
    p.set('since_days', String(days));
    for (const [k, v] of writeStreamWindow(tf)) p.set(k, v);
    void replace(`/events?${p.toString()}`);
    expandedId = null;
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const enc = encodeFilters(filters);
    const s = appliedSearch;
    // The STREAM's window, not the cards' `sinceDays`: this effect exists to
    // discard a cursor that no longer addresses anything, and only the stream's
    // own predicate can invalidate it. Depending on `sinceDays` here would
    // reset the walk every time somebody adjusted a chart's range.
    const tf = streamWindow;
    if (!aid) return;
    // Back to page one, current sort kept. A cursor addresses a position in
    // ONE result set, so it is meaningless against a different predicate —
    // and equally meaningless against a different environment, which is why
    // touching `scopeKey` above has to reset this too and not merely refetch.
    //
    // `list.sort` is read through `untrack`: this effect must depend ONLY on
    // the predicate inputs above, never on `list` itself. `toPage` and
    // `onsort` both write `list` imperatively and reload on their own; if this
    // effect also depended on `list`, either write would re-trigger it and
    // reset the very page move or sort change it just made straight back to
    // page one. Reassigning a `$state` container does not let a read of one
    // untouched field survive the reassignment unnoticed by an effect that
    // reads it — the whole container invalidates, not just the field that
    // changed — so a tracked read here is exactly the self-defeating
    // reset-effect trap (see `svelte5-state-proxy-identity`).
    //
    // Written but never READ REACTIVELY here, and the load takes the fresh
    // list as an argument rather than reading the state back: an effect that
    // depended on `list` would re-run on its own write.
    const next: CursorListState = { sort: untrack(() => list.sort), page: emptyPage() };
    list = next;
    // The walk is over, so the pager goes with it: page one of a new predicate
    // gets the plain spinner it had before any of this, not a pager hovering
    // above it.
    walked = false;
    streamPage = null;
    void loadStream(aid, enc, s, tf, next);
  });

  function selectTopEvent(name: string) {
    const rest = filters.filter((f) => !(f.field === 'name' && f.op === 'eq'));
    filters = [...rest, { field: 'name', op: 'eq', value: name }];
  }

  function toggleRow(id: string) {
    expandedId = expandedId === id ? null : id;
  }

  function propsPreview(props: Record<string, unknown> | null): string {
    if (!props) return '';
    const entries = Object.entries(props);
    if (entries.length === 0) return '';
    return entries.map(([k, v]) => `${k}: ${scalar(v)}`).join('  ·  ');
  }

  function scalar(v: unknown): string {
    if (v === null) return 'null';
    if (Array.isArray(v)) return `[${v.length}]`;
    if (typeof v === 'object') return '{…}';
    const s = String(v);
    return s.length > 48 ? `${s.slice(0, 48)}…` : s;
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Events</h1>
      <p class="muted sub">Product analytics — event volume, top events and raw stream.</p>
    </div>
    <div class="controls">
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  <p class="hint muted">Filter by <code>Tag</code> (key = value); the search box also matches tag &amp; payload content.</p>
  <!--
    `appId`/`context` are what make the search box's autocomplete work at all:
    without them `fetchSchema` bails on its own `if (!appId)` guard and the
    field list silently never loads. This page went without both.
  -->
  <FilterBar
    fields={EVENT_FIELDS}
    bind:filters
    bind:search
    bind:sinceDays
    appId={sessionStore.currentAppId ?? undefined}
    context="events"
    error={searchError}
    {onSearch}
  />
  <SearchDisclosure {clamped} />

  {#if error && top.length === 0 && series.length === 0}
    <Card>
      <EmptyState title="Couldn't load analytics" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) {
                loadTop(aid, sinceDays);
                loadSeries(aid, sinceDays, selectedTopEvent);
              }
            }}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <div class="grid">
      <!--
        Both captions sit OUTSIDE their card's if/else and are rendered empty
        when there is nothing to disclose: the line reserves its own height
        either way, so adding or removing a chip swaps text in place rather
        than resizing a card in a stretch grid — which would move its sibling
        too. Outside the branches for the same reason, so the spinner landing
        does not shift it.

        On the top list an empty `top` is exactly when the caption earns its
        place: "No events yet" under an active filter reads as the filter
        having emptied the list, and it did not.
      -->
      <Card title="Event volume">
        {#if loadingSeries}
          <div class="center"><Spinner size={22} /></div>
        {:else}
          <TimeSeriesChart data={series} height={220} color="var(--primary)" />
        {/if}
        <p class="scope-note">{volumeNote ?? ''}</p>
      </Card>

      <Card title="Top events">
        {#if loadingTop}
          <div class="center"><Spinner size={22} /></div>
        {:else if top.length === 0}
          <EmptyState title="No events yet" description="Send events from your SDK to see them here." icon="chart-column" />
        {:else}
          <p class="hint muted">Click an event to filter the chart and stream.</p>
          <BarList items={top} selected={selectedTopEvent} onselect={selectTopEvent} />
        {/if}
        <p class="scope-note">{topNote ?? ''}</p>
      </Card>
    </div>

    <Card padding="none" title="Event stream">
      {#snippet actions()}
        <!-- Governs the stream only; the two cards above follow the FilterBar's
             range. Placed on the table's own card so the two windows read as
             the separate things they are. -->
        <TimeFilter
          fields={STREAM_TIME_FIELDS}
          value={streamWindow}
          onchange={(v) => (streamWindow = v)}
        />
      {/snippet}
      <!--
        A refresh or retry of THESE rows failed. They stay, and so does the
        pager — losing page 4 of a walk to one bad poll is a worse outcome than
        rows a minute old, and `loadStream` only keeps them when the failed
        request asked for exactly what is on screen. `role="status"`, not
        `alert`: nothing on screen is broken or needs interrupting.
      -->
      {#if staleStreamError}
        <p class="stale-banner" role="status">
          <Icon name="triangle-alert" size={14} />
          <span>Showing the last results that loaded — refreshing failed: {streamError}</span>
          <Button
            variant="ghost"
            size="sm"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) loadStream(aid, encodeFilters(filters), appliedSearch, streamWindow, list);
            }}
          >
            Try again
          </Button>
        </p>
      {/if}
      {#if loadingStream && streamEvents.length === 0}
        <div class="center"><Spinner size={22} /></div>
      {:else if fatalStreamError}
        <EmptyState title="Couldn't load events" description={streamError ?? undefined} icon="triangle-alert">
          {#snippet action()}
            <Button
              variant="secondary"
              onclick={() => {
                const aid = sessionStore.currentAppId;
                if (aid) loadStream(aid, encodeFilters(filters), appliedSearch, streamWindow, list);
              }}
            >
              Retry
            </Button>
          {/snippet}
        </EmptyState>
      {:else if emptyPastFirstPage}
        <!--
          Deliberately not the copy below. That one answers "are there any events
          at all?" and the answer here is yes — `streamTotal` says so in the
          pager underneath. What happened is that this page of the walk no longer
          holds any of them, so "No raw events in this range yet" under a caption
          reading "1,204+ events · Page 4" would be the pager lying in prose.
        -->
        <EmptyState
          title="Nothing left on this page"
          description="These events have gone since the previous page was loaded — the stream moved on, or they fell out of retention. Go back for the ones that are still here."
          icon="search"
        >
          {#snippet action()}
            <Button variant="secondary" onclick={goPrev} disabled={loadingStream}>
              Back a page
            </Button>
          {/snippet}
        </EmptyState>
      {:else if streamEvents.length === 0}
        <EmptyState
          title="No events"
          description={search || filters.length > 0
            ? 'No events match the current filters.'
            : 'No raw events in this range yet.'}
          icon="search"
        />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="name" columnDefault="asc" class="col-name" sort={list.sort} {onsort}>Event</SortableTh>
              <SortableTh key="distinct_id" columnDefault="asc" sort={list.sort} {onsort}>User</SortableTh>
              <SortableTh key="session_id" columnDefault="asc" sort={list.sort} {onsort}>Session</SortableTh>
              <th class="col-props">Properties</th>
              <SortableTh key="occurred_at" class="col-time" sort={list.sort} {onsort}>Time</SortableTh>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each streamEvents as ev (ev.id)}
              <tr class="clickable" onclick={() => toggleRow(ev.id)}>
                <td>
                  <span class="ev-caret" class:open={expandedId === ev.id}><Icon name="chevron-right" size={13} /></span>
                  <span class="ev-name">{ev.name}</span>
                </td>
                <td>
                  {#if ev.distinct_id}
                    <a
                      class="link mono trunc"
                      href={`#/persons/${encodeURIComponent(ev.distinct_id)}`}
                      onclick={(e) => e.stopPropagation()}
                      title={ev.distinct_id}
                    >
                      {ev.distinct_id}
                    </a>
                  {:else}
                    <span class="muted">anonymous</span>
                  {/if}
                </td>
                <td>
                  {#if ev.session_id}
                    <a
                      class="link mono trunc"
                      href={`#/sessions/${encodeURIComponent(ev.session_id)}`}
                      onclick={(e) => e.stopPropagation()}
                      title={ev.session_id}
                    >
                      {ev.session_id}
                    </a>
                  {:else}
                    <span class="faint">—</span>
                  {/if}
                </td>
                <td class="col-props">
                  {#if propsPreview(ev.properties)}
                    <span class="props-prev mono">{propsPreview(ev.properties)}</span>
                  {:else}
                    <span class="faint">—</span>
                  {/if}
                </td>
                <td><TimeValue value={ev.occurred_at} muted /></td>
              </tr>
              {#if expandedId === ev.id}
                <tr class="detail-row">
                  <td colspan={5}>
                    {#if ev.screen}
                      <a
                        class="screen-link mono"
                        href={`#/screens/${encodeURIComponent(ev.screen)}`}
                        onclick={(e) => e.stopPropagation()}
                      >
                        <Icon name="layout-panel-top" size={13} />{ev.screen}
                      </a>
                    {/if}
                    {#if ev.properties && Object.keys(ev.properties).length > 0}
                      <JsonTree value={ev.properties} name="properties" expandTo={2} />
                    {:else}
                      <span class="faint">No properties on this event.</span>
                    {/if}
                    {#if ev.tags && Object.keys(ev.tags).length > 0}
                      <JsonTree value={ev.tags} name="tags" expandTo={2} />
                    {/if}
                    {#if ev.contexts && Object.keys(ev.contexts).length > 0}
                      <JsonTree value={ev.contexts} name="contexts" expandTo={2} />
                    {/if}
                    {#if ev.extra && Object.keys(ev.extra).length > 0}
                      <JsonTree value={ev.extra} name="extra" expandTo={2} />
                    {/if}
                  </td>
                </tr>
              {/if}
            {/each}
          {/snippet}
        </DataTable>
      {/if}

      <!--
        The stand-in count line this replaces ("Showing 50 of 1,204+ events")
        went in when the offset pager was removed and there was nothing to walk
        the list with. The pager carries the same count, so keeping both would
        state it twice.
      -->
      {#if showPager}
        <CursorPagination
          total={streamTotal}
          totalIsCapped={streamTotalCapped}
          page={pageNumber(list.page)}
          limit={STREAM_LIMIT}
          canNext={streamNextCursor !== null}
          busy={loadingStream}
          noun="event"
          {onjump}
        />
      {/if}
    </Card>
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
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .grid {
    display: grid;
    grid-template-columns: 1.6fr 1fr;
    gap: 18px;
    margin-bottom: 18px;
    /* Deliberately NOT `align-items: start`: the volume chart has a fixed 220px
       plot and the top-events list is as tall as its rows, so sizing each to its
       own content leaves the pair ragged. Stretching keeps the row square. */
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 200px;
  }
  .hint {
    font-size: 12px;
    margin-bottom: 12px;
  }
  /* Disclosure line under a header card. `min-height` is the whole trick: the
     element stays in the flow when `panelScopeNote` returns null, so the
     caption appearing is a text swap and never a reflow. */
  .scope-note {
    font-size: 12px;
    line-height: 16px;
    min-height: 16px;
    color: var(--text-faint);
    margin: 10px 0 0;
  }

  /* Sits INSIDE the card, above the rows it qualifies, so the sentence and the
     stale data it describes cannot be read separately. `padding: none` on the
     Card means this supplies its own. Matches Issues'. */
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

  /* Event stream table.
     `.col-name`/`.col-time` are `:global`: those two headers are now
     `SortableTh`, so the `<th>` carrying the class is declared in that
     component's own template, not this one's. Svelte's scoped-CSS hash only
     lands on elements a component's OWN markup declares — never on one
     rendered inside a child it merely handed a class string to — so an
     un-escaped `.col-name { }` here would silently stop matching anything.
     Same fix IssueDetail.svelte already uses via `:global(.occ-table)` to
     reach into `DataTable`. `.col-props` stays scoped: its `<th>` is still
     written directly in this file's own `head` snippet, and its `<td>`s
     always were. */
  :global(.col-name) {
    min-width: 160px;
  }
  .col-props {
    width: 100%;
    max-width: 0;
  }
  :global(.col-time) {
    white-space: nowrap;
  }
  .ev-caret {
    display: inline-block;
    font-size: 8px;
    color: var(--text-faint);
    transition: transform 0.12s ease;
    margin-right: 7px;
  }
  .ev-caret.open {
    transform: rotate(90deg);
  }
  .ev-name {
    font-weight: 560;
    color: var(--text);
  }
  .link {
    color: var(--text);
    text-decoration: none;
    transition: color 0.12s ease;
  }
  .link:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .trunc {
    display: inline-block;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 12px;
  }
  .props-prev {
    display: inline-block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 11.5px;
    color: var(--text-muted);
  }
  .detail-row :global(td) {
    background: var(--surface-2);
    padding: 12px 16px 14px 32px;
  }
  .screen-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-bottom: 10px;
    font-size: 12px;
    font-weight: 550;
    color: var(--primary);
  }
  .screen-link:hover {
    text-decoration: underline;
  }
  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
