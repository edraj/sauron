<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { untrack } from 'svelte';
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
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
  import { OCCURRENCE_FIELDS, encodeFilters, type Filter } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import {
    getIssue,
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
  import { viewCache } from '../lib/stores/view-cache';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    relativeTime,
    formatDateTimeSeconds,
    formatDateTimeZone,
  } from '../lib/utils/format';
  import type { IssueDetail, IssueEventStats, IssueStatus, ErrorEvent } from '../lib/models';

  interface Props {
    params?: { id?: string };
  }
  let { params }: Props = $props();

  let issue = $state<IssueDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let updating = $state(false);

  const issueId = $derived(params?.id ?? '');
  // issues.rs:153 uses the STRICT `authorize_app`, so an env-scoped grant that
  // can read this issue still cannot resolve it.
  const writeLock = $derived(
    lockedBy('issue:write', { app: sessionStore.currentAppId, level: 'app' }),
  );

  async function load(appId: string, id: string) {
    loading = true;
    error = null;
    try {
      issue = await getIssue(appId, id);
    } catch (err) {
      error = errorMessage(err);
      issue = null;
    } finally {
      loading = false;
    }
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

  let occLoading = $state(false);
  let occFilters = $state<Filter[]>([]);
  /** The text in the search box. Editing it queries nothing on its own. */
  let occSearch = $state('');
  /** The query the rows below ran — written only by `onOccSearch`. */
  let occApplied = $state('');
  let occSince = $state(OCC_SINCE_DEFAULT);
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
   * `occSearch` and `occSince` start at rather than by reading those back — a
   * read here would capture the initial value of reactive state and mean
   * nothing, which is precisely what `state_referenced_locally` warns about.
   */
  let occQuery: { enc: string[]; term: string; since: number } = {
    enc: [],
    term: '',
    since: OCC_SINCE_DEFAULT,
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
    since: number,
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
      sinceDays: since,
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

  function plural(n: number, word: string): string {
    return `${formatNumber(n)} ${n === 1 ? word : `${word}s`}`;
  }

  // The search box applies on submit only (button/Enter/clear); the effect
  // below watches `occApplied`, so this is the only thing that runs a query.
  function onOccSearch(q: string) {
    occApplied = q;
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const id = issueId;
    const enc = encodeFilters(occFilters);
    // `occApplied`, never `occSearch`: reading the typed text here is what
    // makes the box fire a request per keystroke.
    const term = occApplied;
    const since = occSince;
    if (!aid || !id) return;
    clearTimeout(occTimer);
    occTimer = setTimeout(() => {
      // Back to page one, current sort kept. A cursor addresses a position in
      // ONE result set, so it is meaningless against a different predicate —
      // and equally meaningless against a different issue or a different
      // environment, which is why `issueId` and `scopeKey` above have to
      // reset this too and not merely refetch.
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

  async function setStatus(next: IssueStatus) {
    const aid = sessionStore.currentAppId;
    const current = issue;
    if (!current || !aid || updating || current.status === next) return;
    const previous = current.status;
    // Optimistic — mutate the reactive $state object in place.
    current.status = next;
    updating = true;
    try {
      const updated = await updateIssueStatus(aid, current.id, next);
      current.status = updated.status;
      current.updated_at = updated.updated_at;
      // The Issues list is cached (see lib/stores/view-cache.ts). Without this,
      // resolving an issue here and navigating back shows it as unresolved for
      // the rest of the fresh window, with no request in flight to correct it —
      // the write looks like it silently failed. Prefix-wide on purpose: the
      // same issue appears under every filter and scope combination.
      viewCache.invalidate('issues.list');
      viewCache.invalidate('issues.stats');
      toastStore.success(`Issue marked ${next}.`);
    } catch (err) {
      current.status = previous;
      toastStore.error(errorMessage(err));
    } finally {
      updating = false;
    }
  }

  const distinctId = $derived(issue?.latest_event?.distinct_id ?? null);
  const eventUserEmail = $derived(
    issue?.latest_event?.event_user?.email ??
      (issue?.latest_event?.context?.user as { email?: string } | undefined)?.email ??
      null,
  );
  const latestEvent = $derived(issue?.latest_event ?? null);
  const latestEventType = $derived(latestEvent?.exception_type ?? issue?.type ?? '');

  // Tags for the rail summary. Same shaping as KeyValueList so the rail and the
  // full-width Tags card can never disagree about what a value looks like.
  const tagEntries = $derived(
    latestEvent?.tags && typeof latestEvent.tags === 'object'
      ? Object.entries(latestEvent.tags)
      : [],
  );
  function renderTag(value: unknown): string {
    if (value === null || value === undefined) return '—';
    if (typeof value === 'object') return JSON.stringify(value);
    return String(value);
  }

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
    return ev.event_user?.email ?? ev.event_user?.username ?? ev.distinct_id ?? 'anonymous';
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

<AppShell requireApp>
  <button class="back" onclick={() => push('/issues')}>
    <Icon name="arrow-left" size={14} />
    {t('issue.backToList')}
  </button>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <EmptyState title={t('issue.error.load')} description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/issues')}>{t('issue.backToList')}</Button>
      {/snippet}
    </EmptyState>
  {:else if issue}
    <header class="detail-head">
      <div class="head-main">
        <div class="badges">
          <span class="type-tag mono">{issue.type}</span>
        </div>
        <h1 class="issue-title">{issue.title}</h1>
        {#if issue.culprit}<p class="culprit mono">{issue.culprit}</p>{/if}
      </div>
        <div class="actions">
          {#if issue.status !== 'resolved'}
            <Button
              variant="primary"
              loading={updating}
              lockedReason={writeLock}
              onclick={() => setStatus('resolved')}
            >
              {t('issue.action.resolve')}
            </Button>
          {/if}
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
              variant="subtle"
              loading={updating}
              lockedReason={writeLock}
              onclick={() => setStatus('unresolved')}
            >
              {t('issue.action.unresolve')}
            </Button>
          {/if}
        </div>
    </header>

    <div class="issue-body">
      <div class="col-main">
        <Card title={t('issue.card.eventsOverTime')}>
          <TimeSeriesChart data={issue.series} height={170} color="var(--error)" />
        </Card>

        {#if latestEvent}
          <Card>
            {#snippet header()}
              <div class="event-head">
                <h3 class="card-title-inline">{t('issue.card.latestEvent')}</h3>
                {#if !metaRedundant}
                  <span class="event-meta mono">{eventMeta}</span>
                {/if}
              </div>
            {/snippet}
            <div class="event-body">
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
              <div class="section">
                <span class="section-label">{t('issue.card.breadcrumbs')}</span>
                <BreadcrumbTrail breadcrumbs={latestEvent.breadcrumbs ?? []} />
              </div>
              <div class="section">
                <span class="section-label">{t('ui.section.context')}</span>
                <KeyValueList data={latestEvent.context} emptyLabel="No context" />
              </div>
            </div>
          </Card>
        {:else}
          <Card title={t('issue.card.latestEvent')}>
            <p class="muted">{t('issue.empty.payload')}</p>
          </Card>
        {/if}

        {#if latestEvent}
          <!-- No Tags card here. The rail already carries one, and two copies of
               the same six rows on one screen made the reader check whether they
               said different things. The rail's is the survivor because it sits
               with the other identity facts (release, environment, affected
               user) instead of below a fold of stack trace and payload; long
               values stay readable there through `title` tooltips. -->
          <div class="data-row">
            <Card title={t('ui.section.contexts')}>
              {#if latestEvent.contexts && Object.keys(latestEvent.contexts).length > 0}
                <JsonTree value={latestEvent.contexts} name="contexts" expandTo={2} />
              {:else}
                <span class="faint">{t('issue.empty.contexts')}</span>
              {/if}
            </Card>

            <Card title={t('ui.section.extra')}>
              {#if latestEvent.extra && Object.keys(latestEvent.extra).length > 0}
                <JsonTree value={latestEvent.extra} name="extra" expandTo={2} />
              {:else}
                <span class="faint">{t('issue.empty.extra')}</span>
              {/if}
            </Card>
          </div>
        {/if}

        <Card title={t('issues.occurrences')}>
          {#snippet actions()}
            {#if occStats}
              <span class="occ-stats" title={t('issue.acrossRange')}>
                {plural(occStats.events, 'event')}
                <span class="sep">·</span>
                {plural(occStats.users, 'user')}
                <span class="sep">·</span>
                {plural(occStats.sessions, 'session')}
              </span>
            {/if}
          {/snippet}
          <!-- This list is occurrences, not issues — the resource decides
               which dimensions the schema advertises. -->
          <FilterBar
            fields={OCCURRENCE_FIELDS}
            bind:filters={occFilters}
            bind:search={occSearch}
            bind:sinceDays={occSince}
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
            <div class="center"><Spinner size={20} /></div>
          {:else if occEmptyPastFirstPage}
            <!--
              Deliberately not the copy below. That one answers "does anything
              match?" and the answer here is yes — the count in the card header
              says so. What happened is that this page of the walk no longer
              holds any of them, so "No occurrences match this filter" under a
              header reading "12,431 events" would be the pager lying in prose.
              No button: Prev is live in the pager directly underneath, which is
              the way back.
            -->
            <p class="faint">
              {t('issue.stale.body')}
            </p>
          {:else if occurrences.length === 0}
            <p class="faint">{t('issue.empty.filtered')}</p>
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
                    <td title={`${relativeTime(ev.occurred_at)} · ${formatDateTimeZone(ev.occurred_at)}`}>
                      <span class="cell-time">{formatDateTimeSeconds(ev.occurred_at)}</span>
                    </td>
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
                        <span class="faint">anonymous</span>
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

      <aside class="rail">
        <Card title={t('issue.card.overview')}>
          <dl class="side-dl">
            <div>
              <dt>{t('common.status')}</dt>
              <dd><StatusBadge status={issue.status} /></dd>
            </div>
            <div>
              <dt>{t('issues.column.level')}</dt>
              <dd><LevelBadge level={issue.level} /></dd>
            </div>
            <div><dt>{t('explore.column.events')}</dt><dd>{formatNumber(issue.times_seen)}</dd></div>
            <div><dt>{t('issue.field.usersAffected')}</dt><dd>{formatNumber(issue.users_seen)}</dd></div>
            <div>
              <dt>{t('explore.column.firstSeen')}</dt>
              <dd><TimeValue value={issue.first_seen} /></dd>
            </div>
            <div>
              <dt>{t('explore.column.lastSeen')}</dt>
              <dd><TimeValue value={issue.last_seen} /></dd>
            </div>
            <div><dt>{t('issue.field.type')}</dt><dd class="mono">{issue.type}</dd></div>
            {#if latestEvent?.release}
              <div><dt>{t('issue.field.release')}</dt><dd class="mono">{latestEvent.release}</dd></div>
            {/if}
            {#if latestEvent?.screen}
              <div>
                <dt>{t('screens.column.screen')}</dt>
                <dd>
                  <a class="screen-link mono" href={`#/screens/${encodeURIComponent(latestEvent.screen)}`}>
                    <Icon name="layout-panel-top" size={13} />{latestEvent.screen}
                  </a>
                </dd>
              </div>
            {/if}
            {#if latestEvent}
              <div>
                <dt>{t('issue.field.occurred')}</dt>
                <dd><TimeValue value={latestEvent.occurred_at} /></dd>
              </div>
            {/if}
            <div>
              <dt>{t('issue.field.fingerprint')}</dt>
              <dd class="mono fp" title={issue.fingerprint}>{issue.fingerprint.slice(0, 16)}…</dd>
            </div>
          </dl>
        </Card>

        <!-- The ONLY Tags card. A second, full-width copy used to sit below the
             stack trace; it was removed rather than this one because tags are
             identity facts and belong beside release/environment/affected user,
             above the fold. Uses `.side-dl` rather than KeyValueList because
             that component lays out for full width; a long value truncates with
             its full text on the `title`. -->
        {#if latestEvent && tagEntries.length > 0}
          <Card title={t('ui.section.tags')}>
            <dl class="side-dl">
              {#each tagEntries as [key, value] (key)}
                <div>
                  <dt class="mono">{key}</dt>
                  <dd class="mono tag-val" title={renderTag(value)}>{renderTag(value)}</dd>
                </div>
              {/each}
            </dl>
          </Card>
        {/if}

        {#if distinctId}
          <Card title={t('ui.section.affectedUser')}>
            <button class="person" onclick={() => push(`/persons/${encodeURIComponent(distinctId)}`)}>
              <span class="p-avatar">{(eventUserEmail ?? distinctId).slice(0, 1).toUpperCase()}</span>
              <span class="p-meta">
                <span class="p-id mono">{distinctId}</span>
                {#if eventUserEmail}<span class="p-email">{eventUserEmail}</span>{/if}
              </span>
              <span class="p-arrow"><Icon name="arrow-right" size={14} /></span>
            </button>
          </Card>
        {/if}
      </aside>
    </div>
  {/if}
</AppShell>

<style>
  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
    padding: 0;
    margin-bottom: 16px;
  }
  .back:hover {
    color: var(--text);
  }
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  /* `.scope-note` lived here for the `payload_searched` line, which now comes
     from `SearchDisclosure` along with the clamp notice — the styling moved
     with it. The reasoning it carried is still true and still applies there:
     no `min-height` is reserved, because this notice sits above a table that
     is itself swapping in from a spinner, so there is no steady state to
     protect, and holding two blank lines under every issue's filter bar for a
     state most members never hit costs more than the reflow it prevents. */
  .detail-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 20px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .badges {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
    flex-wrap: wrap;
  }
  .type-tag {
    font-size: 12px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 3px 9px;
    border-radius: var(--radius-pill);
  }
  .issue-title {
    font-size: 22px;
    font-weight: 660;
    line-height: 1.3;
    word-break: break-word;
  }
  .culprit {
    color: var(--text-muted);
    font-size: 13px;
    margin-top: 6px;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }
  .issue-body {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 22px;
    align-items: start;
  }
  .col-main {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }
  /* Contexts + Additional data sit side by side under the latest-event card. */
  .data-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 18px;
    align-items: start;
  }
  @media (max-width: 640px) {
    .data-row {
      grid-template-columns: 1fr;
    }
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .event-head {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .card-title-inline {
    font-size: 14.5px;
    font-weight: 620;
  }
  .event-meta {
    font-size: 12px;
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
  .side-dl {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  .side-dl > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .side-dl dt {
    font-size: 12px;
    color: var(--text-faint);
  }
  .side-dl dd {
    margin: 0;
    font-size: 12.5px;
    color: var(--text);
    text-align: end;
    word-break: break-word;
  }
  /* Tag values are arbitrary strings in a 300px column. Clamped to two lines
     with the full value on the `title`, so one long value cannot push the rest
     of the rail off screen. The hover text is now the ONLY way to read a
     clamped value in full — the full-width card that used to show them
     untruncated was a duplicate and is gone. */
  .tag-val {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    min-width: 0;
  }
  .fp {
    font-size: 11.5px;
  }
  .screen-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--primary);
    font-size: 12px;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .screen-link:hover {
    text-decoration: underline;
  }
  .person {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    padding: 4px 2px;
    background: none;
    border: none;
    text-align: start;
  }
  .person:hover .p-arrow {
    transform: translateX(3px);
    color: var(--primary);
  }
  .p-avatar {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-weight: 650;
    flex-shrink: 0;
  }
  .p-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .p-id {
    font-size: 12.5px;
    font-weight: 560;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .p-email {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .p-arrow {
    color: var(--text-faint);
    transition: transform 0.14s ease, color 0.14s ease;
  }

  @media (max-width: 900px) {
    .issue-body {
      grid-template-columns: 1fr;
    }
  }

  .faint { color: var(--text-muted); font-size: 12.5px; }

  .occ-stats {
    font-size: 12.5px;
    color: var(--text-muted);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .occ-stats .sep {
    opacity: 0.5;
    margin: 0 2px;
  }
  /* Tabular figures so the stamps form a straight column, and no wrapping —
     a date-time broken across two lines is unreadable at a glance. */
  .cell-time {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
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
