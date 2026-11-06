<script lang="ts">
  import { t, tn, formatNumber } from '../lib/i18n';
  import { untrack } from 'svelte';
  import { push } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import FactList from '../lib/components/FactList.svelte';
  import Fact from '../lib/components/Fact.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import StacktraceView from '../lib/components/StacktraceView.svelte';
  import SymbolicationBadge from '../lib/components/SymbolicationBadge.svelte';
  import BreadcrumbTrail from '../lib/components/BreadcrumbTrail.svelte';
  import KeyValueList from '../lib/components/KeyValueList.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import CursorPagination from '../lib/components/CursorPagination.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import { lastDays, toPredicate, type DateRangeValue } from '../lib/models/date-range';
  import { OCCURRENCE_FIELDS, encodeFilters, type Filter } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import { applyIssueStatus } from '../lib/models/issue-status';
  import {
    getIssueSummary,
    getIssueLatestEvent,
    getIssueSeries,
    updateIssueStatus,
    listIssueEvents,
    getIssueEventStats,
  } from '../lib/api/issues';
  import type { SearchEnvelope, SearchPredicateParams } from '../lib/api/search';
  import {
    canGoBack,
    cursorOf,
    emptyPage,
    offsetOf,
    pageNumber,
  } from '../lib/models/cursor-page';
  import {
    cursorGoTo,
    setCursorSort,
    type CursorListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import { errorMessage, errorStatus } from '../lib/api/client';
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';
  import { preflight, queryErrorFor } from '../lib/utils/query-error';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import type {
    Issue,
    IssueEventStats,
    IssueStatus,
    ErrorEvent,
    SeriesPoint,
  } from '../lib/models';

  interface Props {
    params?: { id?: string };
  }
  let { params }: Props = $props();

  // Cached views (lib/stores/cached-view.svelte.ts): returning to an issue
  // repaints it instantly instead of blanking while the same read runs again.
  //
  // THREE of them, one per section. The record is a keyed lookup; the series
  // and the symbolicated latest event are not. Read as one payload they held
  // the header — the title the reader had just clicked — behind the slowest of
  // the three. Read apart, each card fills in as its own answer lands and shows
  // a skeleton until then. The occurrences table below was always separate.
  const view = new CachedView<Issue>();
  const seriesView = new CachedView<SeriesPoint[]>();
  const latestView = new CachedView<ErrorEvent | null>();

  // `reload()` replays each view's most recent key and fetcher with force,
  // which is exactly what Refresh means here — the page's loads are driven by
  // effects, so there is no `load(force)` to call and reconstructing the key
  // by hand would be a second definition of it, free to drift.
  const refresher = pageRefresher(async () => {
    await Promise.all([view.reload(), seriesView.reload(), latestView.reload()]);
  });

  const issue = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  const revalidating = $derived(
    view.revalidating || seriesView.revalidating || latestView.revalidating,
  );
  // The issue read failing. Mutations (resolve / ignore) report through a
  // toast, never here, so a failed action cannot blank the page.
  const error = $derived(view.error);
  /**
   * A 404 on the record is a fact about this issue, not a transport failure:
   * it gets its own "not found" state with the way back, not "Couldn't load
   * issue" and a Retry that can only 404 again.
   */
  const notFound = $derived(!loading && !!error && view.errorStatus === 404);
  let updating = $state(false);

  const issueId = $derived(params?.id ?? '');
  // issues.rs:153 uses the STRICT `authorize_app`, so an env-scoped grant that
  // can read this issue still cannot resolve it.
  const writeLock = $derived(
    lockedBy('issue:write', { app: sessionStore.currentAppId, level: 'app' }),
  );

  async function load(appId: string, id: string, force = false) {
    // `id` AND `scopeKey`: the router reuses this component across
    // `#/issues/A` -> `#/issues/B`, and `scopeKey` carries the environment the
    // interceptor adds to the request but which appears in no argument here.
    // Two segments only — none of these three routes takes `release=`, so the
    // record is the same under every release; the occurrences table below is
    // the part that narrows, and it keys on `scopeKeyWithRelease`.
    //
    // Started together and NOT awaited as one: each view settles on its own,
    // which is the whole point. `allSettled` only so a caller awaiting `load`
    // resumes once all three have; a failure stays inside its own view.
    await Promise.allSettled([
      view.load(
        viewKey('issue.detail', appId, id, sessionStore.scopeKey),
        () => getIssueSummary(appId, id),
        force,
      ),
      seriesView.load(
        viewKey('issue.detail.series', appId, id, sessionStore.scopeKey),
        () => getIssueSeries(appId, id),
        force,
      ),
      latestView.load(
        viewKey('issue.detail.latest', appId, id, sessionStore.scopeKey),
        () => getIssueLatestEvent(appId, id),
        force,
      ),
    ]);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const id = issueId;
    if (aid && id) void load(aid, id);
  });

  const OCC_LIMIT = 50;
  /** Default occurrence window: 3650d is the backend's effective-"all" cap. */
  const OCC_SINCE_DEFAULT = 3650;
  const OCC_RANGE_DEFAULT: DateRangeValue = lastDays(OCC_SINCE_DEFAULT);

  let occLoading = $state(false);
  let occFilters = $state<Filter[]>([]);
  /** The text in the search box. Editing it queries nothing on its own. */
  let occSearch = $state('');
  /** The query the rows below ran — written only by `onOccSearch`. */
  let occApplied = $state('');
  // NOT seeded from `rangeStore`: this window is the issue's own occurrence
  // history, which defaults to everything. Adopting a 7-day global
  // selection here would open an issue page showing none of its
  // occurrences, which reads as a missing-data bug.
  let occRange = $state<DateRangeValue>(OCC_RANGE_DEFAULT);
  let occStats = $state<IssueEventStats | null>(null);
  let occTimer: ReturnType<typeof setTimeout> | undefined;

  /**
   * The whole `SearchEnvelope` for the occurrences on screen, not just its rows:
   * `next_cursor` describes this exact page, so holding the two together is what
   * stops a later refactor pairing one request's rows with another's cursor.
   *
   * `$state.raw`, not `$state`: it is replaced wholesale on every load and never
   * edited in place, so the deep proxy would be pure overhead.
   */
  let occEnvelope = $state.raw<SearchEnvelope<ErrorEvent> | null>(null);
  const occurrences = $derived(occEnvelope?.data ?? []);
  /** Why the occurrence rows failed, and with what status. */
  let occError = $state<string | null>(null);
  let occErrorStatus = $state<number | null>(null);
  /** The planner's narrowing of this issue's occurrence window. */
  const occClamped = $derived(occEnvelope?.clamped ?? null);

  /** The occurrences schema, held only for `did you mean`. */
  let occSchema = $state<SchemaDefinition | null>(null);
  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    fetchSchema(id, 'occurrences')
      .then((s) => {
        if (!cancelled) occSchema = s;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  /** A local parse problem wins — it means no request was worth issuing. */
  const occSearchError = $derived(
    preflight(occSearch) ?? queryErrorFor(occErrorStatus, occError, occSchema),
  );
  /**
   * The cursor for the NEXT page, read off the envelope that produced the rows
   * rendered — so the Next button's enabled state and the cursor that button
   * sends come from one payload and cannot disagree.
   */
  const occNextCursor = $derived(occEnvelope?.next_cursor ?? null);

  /**
   * Sort and page position for the occurrences table, changed together — see
   * `models/list-state.ts` for why the two live in one field.
   *
   * Moved by a click (`toOccPage`, `onOccSort`), or reset by the debounced
   * reload below — never by a response. `models/cursor-page.ts` explains why
   * that separation is the whole design, and what breaks without it (a reload
   * on page 2 silently stepping the state to page 3 while the rows stay put).
   */
  let occList = $state<CursorListState>({
    sort: { key: 'occurred_at', dir: 'desc' },
    page: emptyPage(),
  });

  /**
   * True once the reader has moved off page one, and until the reload below
   * resets the walk.
   *
   * "Is a walk in progress?" cannot be read off the rows or off `occList.page`
   * during a move: `toOccPage` clears the envelope, so `occurrences.length` is 0 while
   * the request is in flight, and a Prev landing back on page one has
   * `canGoBack` false as well. A pager keyed on either would unmount for the
   * length of every move and remount when it lands, jumping out from under the
   * cursor.
   */
  let occWalked = $state(false);

  /**
   * The pager appears only when there is somewhere else to be — a next page, or
   * a walk already under way.
   *
   * Deliberately NOT `occurrences.length > 0 || occWalked`, which is what Issues
   * and Events use. There the pager's caption replaced an existing count line,
   * so rendering it on a single-page list was net-neutral. Here the card header
   * already states the count (exactly, with the user/session breakdown), so a
   * pager under a 7-row table would add a second copy of that number and two
   * permanently dead buttons to the great majority of issues.
   */
  const showOccPager = $derived(occNextCursor !== null || occWalked);

  /**
   * A landed page with no rows on it, which is not the same fact as "nothing
   * matches" and must not borrow its copy — the stat strip above is a fresh
   * count of the whole match set while the cursor is a boundary from an earlier
   * request, so a retention trim or a deletion between two clicks lands here
   * with "12,431 events" in the header above an empty table.
   */
  const occEmptyPastFirstPage = $derived(
    !occLoading && occurrences.length === 0 && canGoBack(occList.page),
  );

  /**
   * The predicate that produced the rows on screen, so Prev/Next can ask for
   * another page OF THAT RESULT SET.
   *
   * A cursor is only a position within the result set that issued it. This page
   * coalesces the LOAD rather than the inputs, so between a chip change and the
   * reload 250ms later `occFilters` holds a predicate that has not been queried
   * yet — and a Next click reading it would pair the current cursor with a
   * predicate it does not belong to. (`occSearch` has the same gap for a
   * different reason: it is the *typed* text, and only `onOccSearch` promotes
   * it into the queried `occApplied`.)
   *
   * Plain `let`, not `$state`: only the imperative page handlers read it, and
   * nothing renders it. It is seeded from the same constants `occFilters`,
   * `occSearch` and `occRange` start at rather than by reading those back — a
   * read here would capture the initial value of reactive state and mean
   * nothing, which is precisely what `state_referenced_locally` warns about.
   */
  let occQuery: { enc: string[]; term: string; since: DateRangeValue } = {
    enc: [],
    term: '',
    since: OCC_RANGE_DEFAULT,
  };

  /**
   * Out-of-order guard. Paging is a second way to get two requests in flight —
   * a debounced predicate reload and a Prev/Next click can overlap, and nothing
   * about HTTP returns them in order. Only the newest may write, so a slow
   * response for a page the reader has already left cannot land under a pager
   * that has moved on, which would put one page's rows under another's number.
   */
  let occGen = 0;

  async function loadOccurrences(
    appId: string,
    id: string,
    enc: string[],
    term: string,
    since: DateRangeValue,
    l: CursorListState,
  ) {
    const gen = ++occGen;
    occQuery = { enc, term, since };
    occLoading = true;
    // ONE predicate object, handed to BOTH requests below.
    //
    // Typed as `SearchPredicateParams` rather than left inferred so the split is
    // enforced at the point it could be broken: excess-property checking makes
    // it a compile error to slip `limit`/`cursor`/`sort` in here, and the only
    // way to page is therefore to spread this object into the list's arguments.
    // Splitting it into a list-params and a stats-params object is what would
    // let a filter reach one request and not the other, and the symptom is a
    // caption counting a different set than the table shows.
    //
    // `query`, NOT `q` — the same correction Transactions.svelte carries.
    // This box is a query-LANGUAGE input: its placeholder and its autocomplete
    // are generated from the `occurrences` schema, so it offers `release:…`,
    // `@tag.key:value` and `level:[error,fatal]`. Sent as `q` all of that went
    // through the legacy bridge as ONE free-text term and was matched
    // literally, so the box advertised a syntax it then returned zero rows for
    // — verified against a stored occurrence whose `release` is exactly
    // `web@1.0.2`, which `release:web@1.0.2` could not find.
    //
    // Safe on BOTH requests below: `events` and `events/stats` resolve `query=`
    // through one shared `resolve_query` (`routes/issues.rs:EventsQuery`), so
    // the counts cannot end up describing a wider predicate than the rows.
    // `query` still accepts bare free text, so nothing the old spelling did is
    // lost.
    const params: SearchPredicateParams = {
      filters: enc,
      query: term || undefined,
      ...toPredicate(since),
    };
    try {
      // Issued together so the counts and the rows they describe swap in on the
      // same frame; resolving them separately would briefly caption the new
      // rows with the previous filter's totals.
      //
      // `allSettled`, NOT `all`: the counts run `count(DISTINCT …)` over the
      // whole matched range while the list just reads 50 indexed rows, so the
      // stats call is by far the likelier of the two to time out on a large
      // issue. Under `all`, that would reject the pair and blank a perfectly
      // good occurrence table. Losing the stat strip is the acceptable
      // degradation here; losing the rows is not.
      //
      // Only the list moved onto a `SearchEnvelope` in S2c — hence the whole
      // envelope kept on one side and a bare payload on the other.
      // `/events/stats` has no rows to page, so it still answers the bare
      // counts, and those counts stay the ones on screen: the envelope's `total`
      // stops at the server's 10,000 cap, while `events` here is exact.
      //
      // The page half is spread ON TOP of `params` at the call site, never
      // folded into it. That is the whole reason `getIssueEventStats` can take
      // the same object untouched: no ordering and no page boundary changes a
      // total, so the counts describe the entire match set while the list
      // returns one 50-row window of it.
      //
      // Both are re-issued on a page move, the stats included. It is the
      // expensive half, but it is also the self-healing one: a stats request
      // that timed out on the previous load leaves the caption with no count,
      // and re-asking is what puts it back.
      const [rows, stats] = await Promise.allSettled([
        listIssueEvents(appId, id, {
          ...params,
          limit: OCC_LIMIT,
          sort: sortParam(l.sort),
          cursor: cursorOf(l.page),
          offset: offsetOf(l.page),
        }),
        getIssueEventStats(appId, id, params),
      ]);
      if (gen !== occGen) return;
      occEnvelope = rows.status === 'fulfilled' ? rows.value : null;
      occStats = stats.status === 'fulfilled' ? stats.value : null;
      // A rejected rows promise used to be discarded outright, so a query the
      // server REFUSED rendered as "No occurrences match this filter" — an
      // empty state that answers a question nobody got to ask. The reason is
      // kept now, and the search input shows it.
      occError = rows.status === 'rejected' ? errorMessage(rows.reason) : null;
      occErrorStatus = rows.status === 'rejected' ? errorStatus(rows.reason) : null;
    } catch (err) {
      if (gen !== occGen) return;
      occEnvelope = null;
      occStats = null;
      occError = errorMessage(err);
      occErrorStatus = errorStatus(err);
    } finally {
      // Left to the newest call: a superseded one clearing this would drop the
      // spinner while its replacement is still in flight.
      if (gen === occGen) occLoading = false;
    }
  }

  /**
   * Prev/Next/sort load IMPERATIVELY rather than by writing state the reload
   * effect reads back. An effect that both wrote `occList` and read it to
   * build its request would re-run on its own write; this way the effect
   * depends only on the predicate inputs, and neither paging nor sorting ever
   * enters it.
   */
  function toOccPage(next: CursorListState) {
    const aid = sessionStore.currentAppId;
    const id = issueId;
    // The walk does not move unless the request can actually be issued. Written
    // the other way round — state first, request only `if (aid)` — a click with
    // no app selected would step the page number with nothing in flight and no
    // way out but a filter change. `AppShell requireApp` makes that unreachable
    // in practice, so this guards a shape rather than a live bug.
    if (!aid || !id) return;
    occList = next;
    occWalked = true;
    // The rows up are the page being left. Clearing them is what stops the pager
    // labelling one page's rows with another's number while the request is in
    // flight — the card shows its spinner instead, as it does on a filter
    // change. The stat strip is NOT cleared: it is scoped to the predicate, and
    // a page move does not change the predicate.
    occEnvelope = null;
    void loadOccurrences(aid, id, occQuery.enc, occQuery.term, occQuery.since, next);
  }

  /**
   * Move to a numbered page.
   *
   * `cursorGoTo` picks the mechanism — a keyset step when the target is
   * adjacent and a cursor for it exists, an offset jump otherwise — and refuses
   * any move it cannot make by handing back the very `occList` it was given.
   * Testing identity keeps every one of those rules in the reducer, and skips
   * the reload rather than refetching the page already on screen.
   */
  function onOccJump(target: number) {
    const next = cursorGoTo(occList, target, occNextCursor, OCC_LIMIT);
    if (next !== occList) toOccPage(next);
  }

  /**
   * The sort-header click handler passed to every `SortableTh` in the
   * occurrences table. `setCursorSort` resets the walk onto the new ordering —
   * a keyset cursor only addresses a position within the ordering that minted
   * it, so a sort change cannot keep the old page — and this reloads directly
   * for the same reason `toOccPage` does: the debounced reload below must not
   * depend on `occList` (see its comment), so nothing else will notice this
   * write.
   */
  function onOccSort(key: string, columnDefault: SortDir) {
    const aid = sessionStore.currentAppId;
    const id = issueId;
    if (!aid || !id) return;
    const next = setCursorSort(occList, key, columnDefault);
    occList = next;
    occWalked = false;
    occEnvelope = null;
    void loadOccurrences(aid, id, occQuery.enc, occQuery.term, occQuery.since, next);
  }

  // The search box applies on submit only (button/Enter/clear); the effect
  // below watches `occApplied`, so this is the only thing that runs a query.
  function onOccSearch(q: string) {
    occApplied = q;
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKeyWithRelease so the effect re-runs when the environment or
    // the release changes; the interceptor supplies both values, but nothing
    // would refetch without this. The RELEASE half belongs here and not on the
    // issue load above: `…/issues/{id}/events` is in `RELEASE_SCOPED_URL`, so
    // these occurrences narrow to the selected release — while the issue
    // record itself (first/last seen, counts) is app-wide whatever is selected.
    sessionStore.scopeKeyWithRelease;
    const id = issueId;
    const enc = encodeFilters(occFilters);
    // `occApplied`, never `occSearch`: reading the typed text here is what
    // makes the box fire a request per keystroke.
    const term = occApplied;
    const since = occRange;
    if (!aid || !id) return;
    clearTimeout(occTimer);
    occTimer = setTimeout(() => {
      // Back to page one, current sort kept. A cursor addresses a position in
      // ONE result set, so it is meaningless against a different predicate —
      // and equally meaningless against a different issue, environment or
      // release, which is why `issueId` and `scopeKeyWithRelease` above have
      // to reset this too and not merely refetch.
      //
      // `occList.sort` is read through `untrack`, the same rule
      // Events.svelte's equivalent effect follows: this callback must not
      // create a dependency on `occList`, because `toOccPage`/`onOccSort` both
      // write it and reload on their own, and a dependency here would
      // re-trigger this effect on those writes too and undo the very move it
      // made. Reading inside a `setTimeout` callback is already outside any
      // effect's tracking context — see the next paragraph — so this is
      // belt-and-braces, not load bearing, but it keeps the rule legible
      // without relying on that reasoning.
      //
      // Written but never READ by the effect, and the load takes the fresh
      // list as an ARGUMENT rather than reading the state back: an effect
      // that depended on `occList` would re-run on its own write. Belt and
      // braces here, since these writes happen in a timer callback and so are
      // outside the effect's tracking context entirely.
      //
      // The reset sits in the callback rather than in the effect body for a
      // second reason: it must land at the same moment as the rows it describes.
      // Reset synchronously and the 250ms of debounce would show "Page 1" over
      // the page-3 rows still on screen — and a Next click during that window
      // would step the state to page 2 while the pending reload put page one's
      // rows underneath it.
      const next: CursorListState = { sort: untrack(() => occList.sort), page: emptyPage() };
      occList = next;
      // The walk is over, so the pager goes with it: page one of a new predicate
      // gets the plain table it had before any of this, not a pager hovering
      // over it.
      occWalked = false;
      void loadOccurrences(aid, id, enc, term, since, next);
    }, 250);
    return () => clearTimeout(occTimer);
  });

  const STATUS_TOAST = {
    resolved: 'issue.toast.resolved',
    ignored: 'issue.toast.ignored',
    unresolved: 'issue.toast.unresolved',
  } as const satisfies Record<IssueStatus, string>;

  async function setStatus(next: IssueStatus) {
    const aid = sessionStore.currentAppId;
    const current = issue;
    if (!current || !aid || updating) return;
    updating = true;
    try {
      // Optimistic, and by REPLACING the record — see `applyIssueStatus` for
      // why editing `current` in place changed nothing on screen.
      const changed = await applyIssueStatus(view, next, () =>
        updateIssueStatus(aid, current.id, next),
      );
      if (!changed) return;
      // The Issues list is cached (see lib/stores/view-cache.ts). Without this,
      // resolving an issue here and navigating back shows it as unresolved for
      // the rest of the fresh window, with no request in flight to correct it —
      // the write looks like it silently failed. Prefix-wide on purpose: the
      // same issue appears under every filter and scope combination.
      viewCache.invalidate('issues.list');
      viewCache.invalidate('issues.stats');
      toastStore.success(t(STATUS_TOAST[next]));
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      updating = false;
    }
  }

  const latestEvent = $derived(latestView.data ?? null);
  const latestEventType = $derived(latestEvent?.exception_type ?? issue?.type ?? '');

  // Whether the rail's Tags card has anything to show.
  const hasTags = $derived(
    !!latestEvent?.tags &&
      typeof latestEvent.tags === 'object' &&
      Object.keys(latestEvent.tags).length > 0,
  );

  /**
   * Whether the occurrence list is narrowed by a chip or a search — which picks
   * the empty state's wording: "match these filters" when the reader narrowed
   * it, "in this time range" when only the window did.
   */
  const occFiltered = $derived(occFilters.length > 0 || occApplied.trim() !== '');

  const eventMeta = $derived.by(() => {
    const ev = latestEvent;
    if (!ev) return '';
    const body = (ev.exception_value ?? ev.message ?? '').trim();
    // `join` rather than a template: a message-only event has no
    // `exception_type`, and an exception with no value has no body. Either
    // interpolated blind leaves a dangling ": ".
    return [latestEventType, body].filter(Boolean).join(': ');
  });

  /**
   * Whether the red subtitle on "Latest event" merely repeats the page <h1>.
   *
   * It usually does: the heading renders `issue.title`, which the pipeline
   * builds as `"{type}: {value}"` with the value truncated to 200 chars
   * (sauron-pipeline `build_title`), and the subtitle is that same pair at full
   * length.
   *
   * Hence prefix rather than equality — that 200-char cap means the two are
   * rarely byte-identical, and a strict `===` would essentially never fire on
   * the long messages where the duplication is most glaring. The length floor
   * keeps a short heading like "Error" from suppressing an "Error: connection
   * refused" subtitle that does carry new information.
   */
  const metaRedundant = $derived.by(() => {
    const title = squash(issue?.title ?? '');
    const meta = squash(eventMeta);
    if (!title || !meta) return false;
    return title === meta || (meta.startsWith(title) && title.length >= MIN_SHARED_PREFIX);
  });

  const MIN_SHARED_PREFIX = 60;

  // Titles round-trip through Postgres and JSON; compare on collapsed
  // whitespace so a stray newline in a message doesn't defeat the match.
  function squash(s: string): string {
    return s.replace(/\s+/g, ' ').trim();
  }

  // Prefer a name a human recognises, falling back to the id the link points at.
  function userLabel(ev: ErrorEvent): string {
    return (
      ev.event_user?.email ??
      ev.event_user?.username ??
      ev.distinct_id ??
      t('session.meta.anonymous')
    );
  }

  function nested(ctx: Record<string, unknown> | null, group: string, key: string): string | null {
    const g = ctx?.[group];
    if (g == null || typeof g !== 'object') return null;
    const v = (g as Record<string, unknown>)[key];
    return typeof v === 'string' && v !== '' ? v : null;
  }

  // Mirrors how the pipeline derives `device_key` (sauron-pipeline enrich.rs), so
  // the label always describes the device its link resolves to.
  function deviceLabel(ev: ErrorEvent): string | null {
    const c = ev.context;
    const hardware = [nested(c, 'device', 'family'), nested(c, 'device', 'model')]
      .filter(Boolean)
      .join(' ');
    if (hardware) return hardware;
    const os = [nested(c, 'os', 'name'), nested(c, 'os', 'version')].filter(Boolean).join(' ');
    if (os) return os;
    return nested(c, 'runtime', 'name') ?? nested(c, 'ua', 'name');
  }
</script>

  <!-- The title is the id from the URL until the record lands, so the header
       is on screen before any request returns. -->
  <PageHeader
    title={issue?.title ?? issueId}
    titleClass={issue ? '' : 'mono'}
    back={{ label: t('nav.issues'), href: '#/issues' }}
  >
    {#snippet badge()}
      {#if issue}<StatusBadge status={issue.status} />{/if}
    {/snippet}
    {#snippet meta()}
      {#if issue}
        <span class="mono meta-type">{issue.type}</span>
        {#if issue.culprit}<span class="mono meta-culprit">{issue.culprit}</span>{/if}
        <span class="meta-id">
          <span class="mono" title={t('issue.field.fingerprint')}>{issue.fingerprint}</span>
          <CopyButton value={issue.fingerprint} size="sm" />
        </span>
      {/if}
    {/snippet}
    {#snippet freshness()}
      {#if issue}<Freshness fetchedAt={view.fetchedAt} {revalidating} />{/if}
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || view.revalidating} />
    {/snippet}
    {#snippet actions()}
      {#if issue}
        {#if issue.status !== 'ignored'}
          <Button
            variant="secondary"
            loading={updating}
            lockedReason={writeLock}
            onclick={() => setStatus('ignored')}
          >
            {t('issue.action.ignore')}
          </Button>
        {/if}
        {#if issue.status !== 'unresolved'}
          <Button
            variant="ghost"
            loading={updating}
            lockedReason={writeLock}
            onclick={() => setStatus('unresolved')}
          >
            {t('issue.action.unresolve')}
          </Button>
        {/if}
      {/if}
    {/snippet}
    {#snippet primary()}
      {#if issue && issue.status !== 'resolved'}
        <Button
          variant="primary"
          loading={updating}
          lockedReason={writeLock}
          onclick={() => setStatus('resolved')}
        >
          {t('issue.action.resolve')}
        </Button>
      {/if}
    {/snippet}
  </PageHeader>

  <!-- The RECORD decides whether there is a page at all: a 404 or a 403 on it
       is the whole answer, and rendering cards under it would put skeletons
       beneath an issue that does not exist. Everything else is a section of a
       page that does, so each one below waits — and fails — on its own. -->
  {#if notFound}
    <EmptyState
      title={t('issue.notFound.title')}
      description={t('issue.notFound.body')}
      icon="inbox"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/issues')}>{t('issue.backToExceptions')}</Button>
      {/snippet}
    </EmptyState>
  {:else if !loading && error}
    <ErrorState
      title={t('issue.error.load')}
      error={view}
      onretry={() => sessionStore.currentAppId && load(sessionStore.currentAppId, issueId, true)}
    />
  {:else}
    <div class="issue-body">
      <!-- Full width: the series reads better wide than squeezed beside the
           rail. -->
      <Card title={t('issue.card.eventsOverTime')}>
        <ViewSection view={seriesView} rows={1} height="170px">
          <TimeSeriesChart data={seriesView.data ?? []} height={170} color="var(--error)" />
        </ViewSection>
      </Card>

      <!-- The stack trace keeps the full width: frames and file paths run
           long. Breadcrumbs and Context follow beside the Overview rail. -->
      {#if !latestView.hasData}
        <!-- Still out, or failed: `ViewSection` draws whichever it is. Its
             content slot is never reached on this branch. -->
        <Card title={t('issue.card.latestEvent')} collapsible>
          <ViewSection view={latestView} rows={8}>{null}</ViewSection>
        </Card>
      {:else if latestEvent}
        <Card title={t('issue.card.latestEvent')} collapsible>
          <div class="event-body">
            {#if !metaRedundant}
              <p class="event-meta mono" title={eventMeta}>{eventMeta}</p>
            {/if}
            <div class="section">
              <div class="section-head">
                <span class="section-label">{t('ui.section.stacktrace')}</span>
                <SymbolicationBadge
                  status={latestEvent.symbolication_status}
                  isDart={latestEvent.debug_meta?.raw_stacktrace != null}
                />
              </div>
              <StacktraceView
                frames={latestEvent.stacktrace ?? []}
                symbolicated={latestEvent.stacktrace_symbolicated}
                rawTrace={latestEvent.debug_meta?.raw_stacktrace}
              />
            </div>
          </div>
        </Card>
      {:else}
        <Card title={t('issue.card.latestEvent')} collapsible>
          <EmptyState size="section" title={t('issue.empty.payloadTitle')} />
        </Card>
      {/if}

      <!-- Breadcrumbs and Context beside the Overview rail: short rows that
           read next to the identity facts, where the stack trace above needs
           the whole width. Both cards render whatever the latest event's
           state, so the rail never stands beside an empty column; its error
           and retry stay on the Latest event card alone (`failed`). -->
      {#snippet needsLatestEvent()}
        <EmptyState size="section" title={t('issue.empty.needsLatestEvent')} />
      {/snippet}
      <div class="detail-layout">
        <div class="col-main">
          <Card title={t('issue.card.breadcrumbs')} collapsible>
            <ViewSection view={latestView} rows={3} failed={needsLatestEvent}>
              {#if latestEvent}
                <BreadcrumbTrail breadcrumbs={latestEvent.breadcrumbs ?? []} />
              {:else}
                {@render needsLatestEvent()}
              {/if}
            </ViewSection>
          </Card>
          <Card title={t('ui.section.context')} collapsible>
            <ViewSection view={latestView} rows={3} failed={needsLatestEvent}>
              {#if latestEvent}
                <KeyValueList data={latestEvent.context} emptyLabel={t('issue.empty.context')} />
              {:else}
                {@render needsLatestEvent()}
              {/if}
            </ViewSection>
          </Card>
        </div>

        <aside class="rail">
          <!-- Status sits in the header badge and the type and fingerprint in its
               meta line, so they are not repeated here. -->
          <Card title={t('issue.card.overview')}>
            {#if !issue}
              <Skeleton rows={7} />
            {:else}
              <FactList>
                <Fact label={t('issues.column.level')}><LevelBadge level={issue.level} /></Fact>
                <Fact label={t('explore.column.events')}>{formatNumber(issue.times_seen)}</Fact>
                <Fact label={t('issue.field.usersAffected')}>{formatNumber(issue.users_seen)}</Fact>
                <Fact label={t('explore.column.firstSeen')}><TimeValue value={issue.first_seen} /></Fact>
                <Fact label={t('explore.column.lastSeen')}><TimeValue value={issue.last_seen} /></Fact>
                {#if latestEvent?.release}
                  <Fact label={t('issue.field.release')} mono>{latestEvent.release}</Fact>
                {/if}
                {#if latestEvent?.screen}
                  <Fact label={t('screens.column.screen')}>
                    <a class="screen-link mono" href={`#/screens/${encodeURIComponent(latestEvent.screen)}`}>
                      <Icon name="layout-panel-top" size={13} />{latestEvent.screen}
                    </a>
                  </Fact>
                {/if}
                {#if latestEvent}
                  <Fact label={t('issue.field.occurred')}><TimeValue value={latestEvent.occurred_at} /></Fact>
                {/if}
              </FactList>
            {/if}
          </Card>

          <!-- The ONLY Tags card: tags are identity facts, so they sit with
               release and environment in the rail rather than in a second,
               full-width copy among the payload cards. (An "Affected user" card
               used to follow this one; it was dropped because the Occurrences
               list below already names the user on every row.) A raw tag map,
               so it renders through `KeyValueList` like every other one. -->
          {#if latestEvent && hasTags}
            <Card title={t('ui.section.tags')}>
              <KeyValueList data={latestEvent.tags} />
            </Card>
          {/if}
        </aside>
      </div>

      {#if latestEvent}
        <!-- Contexts and Additional data share a row: two payload trees side
             by side, each collapsible on its own. No Tags card among them: two
             copies of the same rows on one screen made the reader check
             whether they said different things, so the rail's is the only one. -->
        <div class="pair">
          <Card title={t('ui.section.contexts')} collapsible>
            {#if latestEvent.contexts && Object.keys(latestEvent.contexts).length > 0}
              <JsonTree value={latestEvent.contexts} name="contexts" expandTo={2} />
            {:else}
              <EmptyState size="section" title={t('issue.empty.contexts')} />
            {/if}
          </Card>

          <Card title={t('ui.section.extra')} collapsible>
            {#if latestEvent.extra && Object.keys(latestEvent.extra).length > 0}
              <JsonTree value={latestEvent.extra} name="extra" expandTo={2} />
            {:else}
              <EmptyState size="section" title={t('issue.empty.extra')} />
            {/if}
          </Card>
        </div>
      {/if}

      <Card title={t('issues.occurrences')}>
        {#snippet actions()}
          {#if occStats}
            <span class="occ-stats" title={t('issue.acrossRange')}>
              {tn('common.plural.event', occStats.events)}
              <span class="sep">·</span>
              {tn('common.plural.user', occStats.users)}
              <span class="sep">·</span>
              {tn('common.plural.session', occStats.sessions)}
            </span>
          {/if}
        {/snippet}
        <!-- This list is occurrences, not issues — the resource decides
             which dimensions the schema advertises. -->
        <FilterBar
          fields={OCCURRENCE_FIELDS}
          bind:filters={occFilters}
          bind:search={occSearch}
          bind:range={occRange}
          appId={sessionStore.currentAppId ?? undefined}
          context="occurrences"
          error={occSearchError}
          onSearch={onOccSearch}
        />
        <!--
          Both notices now come from the shared component, which Issues and
          Events render too. The `payload_searched === false` line originated
          here; the `clamped` one is new everywhere, and this page had the
          same gap as the others — a narrowed window with nothing on screen
          saying so.

          `=== false` and not a truthiness test, still: `null` means no search
          ran at all, and rendering the notice for it would claim a narrowing
          on every unfiltered visit. `disclosuresFor` keeps that distinction.
        -->
        <SearchDisclosure
          clamped={occClamped}
          payloadSearched={occStats?.payload_searched ?? null}
        />
        {#if occLoading}
          <Skeleton rows={6} />
        {:else if occEmptyPastFirstPage}
          <!--
            Deliberately not the copy below. That one answers "does anything
            match?" and the answer here is yes — the count in the card header
            says so. What happened is that this page of the walk no longer
            holds any of them, so "No occurrences match these filters" under a
            header reading "12,431 events" would be the pager lying in prose.
            No button: Prev is live in the pager directly underneath, which is
            the way back.
          -->
          <EmptyState
            size="section"
            title={t('list.stale.title')}
            description={t('list.stale.issuesBody')}
          />
        {:else if occurrences.length === 0}
          {#if occFiltered}
            <EmptyState
              size="section"
              title={t('common.empty.match', { things: t('issue.things.occurrences') })}
              description={t('common.empty.matchHint')}
            />
          {:else}
            <EmptyState
              size="section"
              title={t('common.empty.range', { things: t('issue.things.occurrences') })}
              description={t('common.empty.rangeHint')}
            />
          {/if}
        {:else}
          <DataTable class="occ-table">
            {#snippet head()}
              <tr>
                <SortableTh key="occurred_at" sort={occList.sort} onsort={onOccSort}>{t('events.column.time')}</SortableTh>
                <SortableTh key="distinct_id" columnDefault="asc" sort={occList.sort} onsort={onOccSort}>{t('sessions.column.user')}</SortableTh>
                <SortableTh key="session_id" columnDefault="asc" sort={occList.sort} onsort={onOccSort}>{t('sessions.column.session')}</SortableTh>
                <SortableTh key="device_key" columnDefault="asc" sort={occList.sort} onsort={onOccSort}>{t('sessions.column.device')}</SortableTh>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each occurrences as ev (ev.id)}
                <tr>
                  <td><TimeValue value={ev.occurred_at} /></td>
                  <td>
                    {#if ev.distinct_id}
                      <a
                        class="link trunc"
                        href={`#/persons/${encodeURIComponent(ev.distinct_id)}`}
                        title={userLabel(ev)}
                      >
                        {userLabel(ev)}
                      </a>
                    {:else}
                      <span class="faint">{t('session.meta.anonymous')}</span>
                    {/if}
                  </td>
                  <td>
                    {#if ev.session_id}
                      <a
                        class="link cell-mono trunc"
                        href={`#/sessions/${encodeURIComponent(ev.session_id)}`}
                        title={ev.session_id}
                      >
                        {ev.session_id}
                      </a>
                    {:else}
                      <span class="faint">—</span>
                    {/if}
                  </td>
                  <td>
                    {#if ev.device_key}
                      <a
                        class="link trunc"
                        href={`#/devices/${encodeURIComponent(ev.device_key)}`}
                        title={deviceLabel(ev) ?? ev.device_key}
                      >
                        {deviceLabel(ev) ?? ev.device_key}
                      </a>
                    {:else if deviceLabel(ev)}
                      <span class="trunc">{deviceLabel(ev)}</span>
                    {:else}
                      <span class="faint">—</span>
                    {/if}
                  </td>
                </tr>
              {/each}
            {/snippet}
          </DataTable>
        {/if}

        <!--
          The caption counts `occStats.events`, NOT the envelope's `total`.
          They describe the same set — `event_stats` runs the identical
          predicate over the identical window as the list, and its `events` is
          a `count(*)` of the very rows being paged — but the envelope's stops
          at the server's 10,000 cap while this one is exact. That is why
          `totalIsCapped` is a flat `false`: it states a fact about this
          number, not a placeholder. Swapping in `occEnvelope.total` would
          silently turn an exact count into a lower bound, and put a second,
          disagreeing figure a few rows under the one in the card header.

          `null` when the stats half of the `allSettled` pair failed — the
          degradation that pair exists for — which is exactly the "no count to
          state" the control renders as a bare page number.
        -->
        {#if showOccPager}
          <CursorPagination
            total={occStats?.events ?? null}
            totalIsCapped={false}
            page={pageNumber(occList.page)}
            limit={OCC_LIMIT}
            canNext={occNextCursor !== null}
            busy={occLoading}
            noun="occurrence"
            onjump={onOccJump}
          />
        {/if}
      </Card>
    </div>
  {/if}

<style>
  /* `.scope-note` lived here for the `payload_searched` line, which now comes
     from `SearchDisclosure` along with the clamp notice — the styling moved
     with it. The reasoning it carried is still true and still applies there:
     no `min-height` is reserved, because this notice sits above a table that
     is itself swapping in from a spinner, so there is no steady state to
     protect, and holding two blank lines under every issue's filter bar for a
     state most members never hit costs more than the reflow it prevents. */
  .meta-type,
  .meta-culprit {
    font-size: var(--text-2xs);
    overflow-wrap: anywhere;
  }
  .meta-id {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: var(--text-2xs);
    overflow-wrap: anywhere;
  }
  /* The rail row is the shared `.detail-layout` (app.css). */
  .issue-body {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
    min-width: 0;
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--page-gap);
    align-items: start;
  }
  .col-main {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
    min-width: 0;
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }
  .event-meta {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--error);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .event-body {
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .section {
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  /* `.sym-badge` moved to lib/components/SymbolicationBadge.svelte, which the
     session timeline shares. */
  .screen-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--primary);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .screen-link:hover {
    text-decoration: underline;
  }
  /* Same breakpoint as `.detail-layout`. */
  @media (max-width: 900px) {
    .pair {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .occ-stats {
    font-size: var(--text-xs);
    color: var(--text-muted);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .occ-stats .sep {
    opacity: 0.5;
    margin: 0 2px;
  }

  :global(.occ-table) {
    margin-top: 8px;
  }
  /* Ids can be long; keep each identity column bounded so no single cell pushes
     the table into horizontal scroll. */
  .trunc {
    display: inline-block;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: bottom;
  }
  .link {
    color: var(--primary);
    text-decoration: none;
  }
  .link:hover {
    text-decoration: underline;
  }
</style>
