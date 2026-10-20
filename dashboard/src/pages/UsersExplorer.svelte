<script lang="ts">
  import { untrack } from 'svelte';
  import { t, localeStore, intlTag } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import { rowHref, rowNav } from '../lib/utils/row-link';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import ListToolbar from '../lib/components/ListToolbar.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import { formatAbsolute, spanDays, type DateRangeValue } from '../lib/models/date-range';
  import TimeFilter from '../lib/components/TimeFilter.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import RollupChip from '../lib/components/ui/RollupChip.svelte';
  import { refreshRollups } from '../lib/api/rollups';
  import { approx } from '../lib/models/freshness';
  import { rollupState } from '../lib/stores/rollups.svelte';
  import UserActivityChart from '../lib/components/UserActivityChart.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { countPersons } from '../lib/api/counts';
  import { RowCount } from '../lib/stores/row-count.svelte';
  import { listPersons } from '../lib/api/persons';
  import { getUserAnalytics } from '../lib/api/users';
  import { errorCode, errorMessage } from '../lib/api/client';
  import {
    RELEASE_MODES,
    RELEASE_MODE_PARAM,
    effectiveReleaseMode,
    isReleaseRollupPending,
    parseReleaseMode,
    releaseModeParams,
    writeReleaseModeParam,
    type ReleaseMode,
  } from '../lib/models/release-users';
  import type { MessageKey } from '../lib/i18n';
  import {
    setOffsetPage,
    setOffsetSort,
    type ListPage,
    type OffsetListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import {
    fromParams,
    toParams,
    toRecord,
    type TimeField,
    type TimeFilterState,
  } from '../lib/models/time-filter';
  import {
    initials,
    hueFromString,
    compactNumber,
    formatDuration,
    formatPercent,
  } from '../lib/utils/format';
  import type { PersonRow, UsersAnalytics } from '../lib/models';

  const LIMIT = 50;

  /**
   * The two columns this table can be windowed by.
   *
   * `first_seen` is what makes "new users" askable: a person first seen 90 days
   * ago but active yesterday matches a `last_seen` window and misses a
   * `first_seen` one. The stat tiles above have always drawn that distinction
   * ("Active" vs "New") over a table that could not reproduce either.
   */
  const TIME_FIELDS: TimeField[] = [
    { key: 'last_seen', label: 'Last seen' },
    { key: 'first_seen', label: 'First seen' },
  ];
  const DEFAULT_TIME_FIELD = 'last_seen';

  /**
   * 365, not the 30 the other lists default to.
   *
   * This table has never had a time window at all — the range picker on this
   * page drove only the stat tiles — so it has always shown every person.
   * Defaulting to 30 days would make most of an app's users vanish from a list
   * that has always shown them all, as a side effect of a filter nobody
   * touched. 365 is the route's own ceiling, so it is the widest honest default
   * available.
   */
  const DEFAULT_DAYS = 365;

  let searchTerm = $state('');
  let query = $state('');

  // Restored from the URL on first paint so a shared or refreshed link keeps
  // its window. An unparseable or unoffered value degrades to the default
  // rather than 400ing — see `fromParams`.
  const initial = new URLSearchParams($querystring ?? '');
  let timeFilter = $state<TimeFilterState>(
    fromParams(initial, TIME_FIELDS, DEFAULT_TIME_FIELD, DEFAULT_DAYS),
  );

  /**
   * "Used it" vs "Currently on it" — which people a picked release means for
   * the list and its count (`models/release-users.ts`). Restored from the URL
   * like the window above; an unknown value degrades to the default.
   *
   * Kept while no release is picked rather than reset, but not in force then:
   * `effectiveMode` is `null`, the toggle is not rendered, the requests carry
   * no `release_mode` and the view keys are the release-blind ones.
   */
  let releaseMode = $state<ReleaseMode>(parseReleaseMode(initial.get(RELEASE_MODE_PARAM)));
  const effectiveMode = $derived(effectiveReleaseMode(releaseMode, sessionStore.currentRelease));
  /**
   * Whether the sketch-derived tiles (DAU/WAU/MAU, median session) carry the
   * ≈ mark. The release-blind summary is sketch-served once the base rollups
   * are ready; a release-scoped one ALWAYS is (`user_activity_release_daily`,
   * `session_stats_release_daily` — there is no exact fallback), so a picked
   * release marks them regardless of `rollupState`.
   */
  const sketchServed = $derived(rollupState.ready || sessionStore.currentRelease !== null);

  const RELEASE_MODE_TEXT: Record<ReleaseMode, { label: MessageKey; title: MessageKey }> = {
    used: { label: 'users.releaseMode.used', title: 'users.releaseMode.usedTitle' },
    current: { label: 'users.releaseMode.current', title: 'users.releaseMode.currentTitle' },
  };

  function setReleaseMode(next: ReleaseMode) {
    if (next === releaseMode) return;
    releaseMode = next;
    // A different set of people: row 51 of "used it" is not row 51 of
    // "currently on it", same as for a changed window.
    list = setOffsetPage(list, 0);
  }

  // `last_seen` descending is the endpoint's own default, so this describes
  // the first request rather than changing it.
  let list = $state<OffsetListState>({ sort: { key: 'last_seen', dir: 'desc' }, offset: 0 });

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  // Cached view (lib/stores/cached-view.svelte.ts): rows already fetched paint
  // instantly on return, then refresh behind a spinner instead of a skeleton.
  // Re-exposed under the names the template already used, so the markup is
  // unchanged apart from the refresh indicator.
  const view = new CachedView<ListPage<PersonRow>>();
  const rows = $derived(view.data?.rows ?? []);
  // Read off the cached payload, not a separate `$state` set on the network
  // path: a cache HIT repaints rows without fetching, and a `hasNext` only the
  // fetch updates would be the previous key's answer.
  const hasNext = $derived(view.data?.hasNext ?? false);
  const rowCount = new RowCount();
  const revalidating = $derived(view.revalidating);
  const loading = $derived(view.loading);
  const error = $derived(view.error);

  let range = $state<DateRangeValue>(rangeStore.effective(30));
  /** The window in words, under the tiles it applies to. */
  /**
   * Three rows under a people count: the window, then "N identified" and
   * "M guests". Identified means the `event_users` row has been named by
   * `identify()` or a matching context user id; guests are the remainder.
   * `identified` is `null` when the server cannot split (dau/wau/mau on a
   * rollup window with a day whose identified sketch is not built yet) — say
   * so rather than show every active person as a guest.
   */
  function split(caption: string, total: number, identified: number | null): string[] {
    if (identified === null) return [caption, t('users.splitUnknown')];
    const guests = Math.max(0, total - identified);
    return [
      caption,
      `${compactNumber(identified)} ${t('users.identifiedShort')}`,
      `${compactNumber(guests)} ${t('users.guestsShort')}`,
    ];
  }

  const rangeCaption = $derived(
    range.kind === 'last'
      ? `last ${spanDays(range)}d`
      : formatAbsolute(range, intlTag(localeStore.locale)),
  );
  let analytics = $state<UsersAnalytics | null>(null);
  let analyticsError = $state<string | null>(null);

  let refreshing = $state(false);

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      // Kick an immediate rollup fold first (bounded server-side wait), so
      // the reloads below fetch aggregates that include the newest events.
      // Older APIs 404 this — then the reload alone is the refresh.
      await refreshRollups(aid).catch(() => {});
      await Promise.all([
        load(aid, query, sortParam(list.sort), list.offset, timeFilter, effectiveMode, true),
        loadAnalytics(aid, range),
      ]);
    } finally {
      refreshing = false;
    }
  }

  // Wraps this page's OWN refresh rather than replacing it: the body below
  // does page-specific work a generic sweep would drop. `pageRefresher` adds
  // the server force window and the wait for the recompute to land, so a page
  // button and the one in the top bar now mean the same thing.
  const refresher = pageRefresher(refresh);

  /**
   * The summary answered 503 `release_rollup_pending`: this app's per-release
   * tables are still being backfilled. Rendered as an explanation, not as
   * `analyticsError` — nothing is broken, and the numbers arrive on their own.
   */
  let analyticsPending = $state(false);
  /**
   * Only the newest summary request may write. A release (or environment, or
   * window) switch fires a new one before the previous settles, and the slow
   * all-release answer landing after the per-release one would put the wrong
   * release's numbers under the switcher with nothing on screen to say so.
   */
  let analyticsGen = 0;

  async function loadAnalytics(appId: string, win: DateRangeValue) {
    const gen = ++analyticsGen;
    analyticsError = null;
    analyticsPending = false;
    try {
      const next = await getUserAnalytics(appId, win);
      if (gen !== analyticsGen) return;
      analytics = next;
    } catch (err) {
      if (gen !== analyticsGen) return;
      analytics = null;
      if (isReleaseRollupPending(errorCode(err))) analyticsPending = true;
      else analyticsError = errorMessage(err);
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKeyWithRelease so the effect re-runs when the environment or
    // the release changes: `…/users/summary` is release-scoped (the
    // interceptor attaches both values), so every tile and the chart follow
    // the switcher — but nothing would refetch without this.
    sessionStore.scopeKeyWithRelease;
    const win = range;
    if (aid) void loadAnalytics(aid, win);
  });

  // Submit-driven, not debounced: `searchTerm` is the text in the box and
  // `query` is what the request carries, and only the Search button, Enter and
  // the clear button move one into the other. Typing reaches the network
  // nowhere on this page.
  function onSearch(v: string) {
    query = v.trim();
    // A changed predicate invalidates the page position: row 51 of the old
    // result set is not row 51 of the new one.
    list = setOffsetPage(list, 0);
  }

  // `scopeKeyWithRelease` must be in the key: it carries the selected
  // environment and release, which the axios interceptor adds to the request
  // but which appear in none of these arguments. Omit it and one environment's
  // (or release's) rows are served as another's. `mode` is the release mode in
  // force (`null` with no release picked), in the key for the same reason.
  //
  // `sort` is in the cache key for the same reason `q` and `off` are: without
  // it a header click finds the previous ordering already cached under the
  // same key and repaints it with NO request on the wire, so the sort looks
  // like it silently did nothing.
  //
  // `force` bypasses the fresh-window short-circuit — an explicit Refresh or
  // Retry means "go to the network now".
  async function load(
    appId: string,
    q: string,
    sort: string,
    off: number,
    tf: TimeFilterState,
    mode: ReleaseMode | null,
    force = false,
  ) {
    // The window is in the key for the same reason `q`, `sort` and `off` are:
    // without it, changing the filter finds the previous window's rows already
    // cached under the same key and repaints them with NO request on the wire,
    // so the filter looks like it silently did nothing.
    //
    // It enters the key as the filter's DECLARATION (`last:365:`), never as the
    // instant `last` resolves to. A clock-derived value in a `viewKey` mints a
    // fresh entry on every single load — the cache stays wired, typed and green
    // while hitting zero times, and nothing in the DOM shows it. Only the
    // network panel does.
    const windowKey = `${tf.field}:${tf.mode}:${tf.lastDays ?? ''}:${tf.from ?? ''}:${tf.to ?? ''}`;
    // Predicate only — `sort`, `off` and `LIMIT` are deliberately absent, so
    // paging and reordering never refetch a number that cannot have changed.
    void rowCount.load(
      viewKey('persons.count', appId, sessionStore.scopeKeyWithRelease, q, windowKey, mode),
      () =>
        countPersons(appId, {
          search: q || undefined,
          window: toRecord(tf, DEFAULT_TIME_FIELD),
          releaseMode: mode ?? undefined,
        }),
      force,
    );
    await view.load(
      viewKey(
        'persons.list',
        appId,
        sessionStore.scopeKeyWithRelease,
        q,
        sort,
        off,
        LIMIT,
        windowKey,
        mode,
      ),
      () => listPersons(appId, {
        search: q || undefined,
        sort,
        limit: LIMIT,
        offset: off,
        ...toRecord(tf, DEFAULT_TIME_FIELD),
        ...releaseModeParams(mode),
      }),
      force,
    );
  }

  // A different environment or release is a different result set: page 3 of
  // one release is not page 3 of the next (and may not exist), so go back to
  // page one — the Events page's rule. Declared BEFORE the load effect below
  // so that effect reads the reset offset and fetches once. `list` is read
  // through `untrack`: a page move or sort change must not re-trigger this
  // and undo itself.
  let lastScope: string | null = null;
  $effect(() => {
    const scope = sessionStore.scopeKeyWithRelease;
    if (lastScope !== null && scope !== lastScope) {
      untrack(() => {
        if (list.offset !== 0) list = setOffsetPage(list, 0);
      });
    }
    lastScope = scope;
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKeyWithRelease so the effect re-runs when the environment or
    // the release changes; the interceptor supplies both values, but nothing
    // would refetch without this.
    sessionStore.scopeKeyWithRelease;
    const q = query;
    const sort = sortParam(list.sort);
    const off = list.offset;
    const tf = timeFilter;
    const mode = effectiveMode;
    if (aid) void load(aid, q, sort, off, tf, mode);
  });

  /** The list answered 503 `release_rollup_pending` — see `analyticsPending`. */
  const listPending = $derived(error !== null && isReleaseRollupPending(view.errorCode));
  /**
   * The Audience section is showing the full still-building explanation, or is
   * about to (its summary is in flight — both reads share one readiness gate
   * server-side). The people section then shows the title alone instead of
   * repeating the paragraph and the button, and does not flash them while a
   * "Check again" has the summary reloading.
   */
  const pendingExplainedAbove = $derived(
    analyticsPending || (analytics === null && analyticsError === null),
  );

  /**
   * "Check again" on the still-building state: repeat whichever of the two
   * reads is waiting, past the fresh window. Not `refresh()` — that also kicks
   * a rollup fold, which has nothing to do with the release backfill.
   */
  function checkAgain() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    if (analyticsPending) void loadAnalytics(aid, range);
    if (listPending) {
      void load(aid, query, sortParam(list.sort), list.offset, timeFilter, effectiveMode, true);
    }
  }

  // The URL mirrors the window so a filtered view survives a refresh and can be
  // shared. `replace`, not `push`: adjusting a filter is not a navigation, and
  // a history entry per keystroke-equivalent would make Back useless.
  //
  // This writes the query string that `$querystring` feeds, so it must not be
  // read back into `timeFilter` — `initial` is read ONCE at setup, above,
  // rather than through a `$derived`, which is what keeps this from looping.
  //
  // The release mode rides along only while it is in force and not the default
  // — see `writeReleaseModeParam`.
  $effect(() => {
    const p = writeReleaseModeParam(toParams(timeFilter, DEFAULT_TIME_FIELD), effectiveMode);
    const qs = p.toString();
    void replace(qs ? `/users?${qs}` : '/users');
  });

  // A compact digest of the most useful person traits, shown in the table.
  const TRAIT_KEYS = ['email', 'plan', 'name'];

  function traits(props: Record<string, unknown> | null): { key: string; value: string }[] {
    if (!props) return [];
    const out: { key: string; value: string }[] = [];
    for (const key of TRAIT_KEYS) {
      const v = props[key];
      if (v !== undefined && v !== null && v !== '') {
        out.push({ key, value: typeof v === 'object' ? JSON.stringify(v) : String(v) });
      }
    }
    return out;
  }

  // The traits the "…" chip stands in for, as a hover title. The chip replaces
  // them in the cell, so without this the values would be unreachable without
  // opening the person — a compaction that loses data rather than folding it.
  function traitSummary(rest: { key: string; value: string }[]): string {
    return rest.map((tag) => `${tag.key}: ${tag.value}`).join('\n');
  }

  function personPath(distinctId: string): string {
    return '/persons/' + encodeURIComponent(distinctId);
  }
</script>

  <div class="head">
    <div>
      <h1 class="page-title">{t('users.title')}</h1>
      <p class="muted sub">{t('users.subtitle')}</p>
    </div>
  </div>

  <div class="analytics-head">
    <div>
      <h2 class="section-title">{t('users.audience')}</h2>
      <p class="muted sub">
        {t('users.thisAppOnly')} <a href="#/active-users">{t('users.combinedActive')}</a> {t('prose.users.combinedNoteTail')}
      </p>
    </div>
    <DateRange
      value={range}
      onchange={(v) => {
        range = v;
        rangeStore.set(v);
      }}
    />
  </div>

  <!-- The spacing lives on this wrapper rather than on `StatTiles` / `Card`:
       both are shared house components, and a margin given to them from a page
       would follow them onto every other page that uses them. -->
  <div class="audience">
    {#if analytics}
      <StatTiles min={150}>
        <StatTile label={t('users.stat.total')} value={compactNumber(analytics.stats.total_users)} tone="primary" sub={split('all time', analytics.stats.total_users, analytics.stats.total_identified)} />
        <StatTile label={t('users.stat.active')} value={compactNumber(analytics.stats.active_in_range)} sub={split(rangeCaption, analytics.stats.active_in_range, analytics.stats.active_identified)} />
        <StatTile label={t('users.stat.new')} value={compactNumber(analytics.stats.new_in_range)} sub={split(rangeCaption, analytics.stats.new_in_range, analytics.stats.new_identified)} />
        <!-- `stats.dau` has always been in the payload and in the `UserStats`
             model; the tile was simply never rendered, which is why this page
             shows a stickiness ratio whose numerator is invisible. -->
        <StatTile label="DAU" value={approx(compactNumber(analytics.stats.dau), sketchServed)} sub={split('24h', analytics.stats.dau, analytics.stats.dau_identified)} />
        <StatTile label="WAU" value={approx(compactNumber(analytics.stats.wau), sketchServed)} sub={split('7-day', analytics.stats.wau, analytics.stats.wau_identified)} />
        <StatTile label="MAU" value={approx(compactNumber(analytics.stats.mau), sketchServed)} sub={split('30-day', analytics.stats.mau, analytics.stats.mau_identified)} />
        <StatTile label={t('users.stat.stickiness')} value={formatPercent(analytics.stickiness)} sub="DAU / MAU" />
        <StatTile label={t('sessions.stat.avg')} value={formatDuration(analytics.stats.avg_session_ms)} />
        <StatTile label={t('sessions.stat.median')} value={approx(formatDuration(analytics.stats.median_session_ms), sketchServed)} />
      </StatTiles>

      <Card title={t('users.card.activePerDay')}>
        <UserActivityChart data={analytics.series} />
      </Card>
    {:else if analyticsPending}
      <Card>{@render releasePending(true)}</Card>
    {:else if analyticsError}
      <Card><p class="muted">{analyticsError}</p></Card>
    {:else}
      <Skeleton rows={2} height="70px" label={t('users.loading.stats')} />
      <Card>
        <Skeleton rows={1} height="200px" label={t('users.loading.chart')} />
      </Card>
    {/if}
  </div>

  <!-- Gives the table the same section identity as Audience above it. Without a
       heading the widened gap reads as a stray hole rather than a boundary, and
       the table's own header row has to double as the section label. Sits
       outside the branch below so it holds through the loading and empty
       states instead of appearing only once rows land. -->
  <div class="people-head">
    <div>
      <h2 class="section-title">{t('users.people')}</h2>
      <!-- No longer "most recently seen first": that was true of the fixed
           ordering this table used to have, and would now contradict the
           header the user just clicked. -->
      <p class="muted section-hint">{t('users.onePerDistinctId')}</p>
    </div>
  </div>

  <!-- Every control that narrows the TABLE lives in this one row, directly
       above it: search, window, refresh — the same `ListToolbar` every list
       page renders, so the box is where Sessions and Devices put theirs. They
       used to be split between the page header and here, which read as two
       unrelated toolbars and left the search box describing a table two
       sections further down. -->
  <ListToolbar>
    {#snippet searchBox()}
      <SearchInput bind:value={searchTerm} onsearch={onSearch} placeholder={t('users.search')} />
    {/snippet}
    {#snippet timeWindow()}
      <!-- Only while a release is picked: without one there is nothing for
           "used it" or "currently on it" to refer to. Part of the predicate,
           like the window beside it, so it sits in the same slot. -->
      {#if sessionStore.currentRelease !== null}
        <div class="release-mode" role="group" aria-label={t('users.releaseMode.label')}>
          {#each RELEASE_MODES as m (m)}
            <button
              type="button"
              class="mode"
              class:active={releaseMode === m}
              aria-pressed={releaseMode === m}
              title={t(RELEASE_MODE_TEXT[m].title)}
              onclick={() => setReleaseMode(m)}
            >
              {t(RELEASE_MODE_TEXT[m].label)}
            </button>
          {/each}
        </div>
      {/if}
      <!-- Governs the TABLE only. The Audience range picker above drives the
           tiles and chart, and the two are deliberately separate windows: this
           one can name a column and a bound the summary endpoints cannot
           express, so a shared control would have to either lie or caption its
           way out on every card. -->
      <TimeFilter
        fields={TIME_FIELDS}
        value={timeFilter}
        onchange={(v) => {
          timeFilter = v;
          // A changed predicate invalidates the page position: row 51 of the old
          // window is not row 51 of the new one, and keeping the offset would
          // land the user in the middle of a result set they have not seen the
          // start of — or past its end, on an empty page.
          list = setOffsetPage(list, 0);
        }}
      />
    {/snippet}
    {#snippet actions()}
      <RollupChip />
      <!-- One stamp per page: while rollups serve it, `RollupChip`'s fold
           watermark IS the data's age and this fetch-time chip would be a
           second "Updated" beside it with a different time. -->
      {#if !rollupState.ready}
        <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
      {/if}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || refreshing || revalidating} />
    {/snippet}
  </ListToolbar>

  {#if loading && rows.length === 0}
    <Skeleton rows={8} height="48px" label={t('users.loading.users')} />
  {:else if listPending}
    <!-- The full explanation once per page: when the Audience section above
         carries it, this one is the title alone. -->
    <Card>{@render releasePending(!pendingExplainedAbove)}</Card>
  {:else if error}
    <Card>
      <EmptyState title={t('users.error.load')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, query, sortParam(list.sort), list.offset, timeFilter, effectiveMode);
            }}
          >
            {t('common.retry')}
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if rows.length === 0 && !query && sessionStore.currentRelease !== null}
    <!-- The release-blind copy below ("users appear once your SDK identifies
         people") is false here: the app has users, none of them on this
         release in this window. -->
    <Card>
      <EmptyState
        title={t('users.empty.releaseTitle')}
        description={t('users.empty.releaseBody')}
        icon="package"
      />
    </Card>
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title={query ? 'No matching users' : 'No users yet'}
        description={query
          ? `Nothing matched “${query}”. Try a different distinct ID or trait.`
          : 'Users appear once your SDK identifies people or sends events with a distinct ID.'}
        icon="user"
      />
    </Card>
  {:else}
    <div class="table" class:loading>
      <DataTable>
        {#snippet head()}
          <tr>
            <SortableTh key="distinct_id" columnDefault="asc" sort={list.sort} {onsort}>
              {t('sessions.column.user')}
            </SortableTh>
            <!-- Traits stays a plain `<th>`: the cell is a fold of an
                 arbitrary JSON object into one chip plus a "…", so there is no
                 single column to order by and the endpoint offers none. -->
            <th>{t('users.column.traits')}</th>
            <SortableTh key="sessions_count" class="num" sort={list.sort} {onsort}>
              {t('explore.column.sessions')}
            </SortableTh>
            <SortableTh key="events_count" class="num" sort={list.sort} {onsort}>
              {t('explore.column.events')}
            </SortableTh>
            <SortableTh key="errors_count" class="num" sort={list.sort} {onsort}>
              {t('explore.column.errors')}
            </SortableTh>
            <SortableTh key="first_seen" sort={list.sort} {onsort}>{t('explore.column.firstSeen')}</SortableTh>
            <SortableTh key="last_seen" sort={list.sort} {onsort}>{t('explore.column.lastSeen')}</SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as row (row.distinct_id)}
            {@const rowTraits = traits(row.properties)}
            {@const path = personPath(row.distinct_id)}
            <tr
              class="clickable"
              onclick={(e) => rowNav(e, path)}
              onauxclick={(e) => rowNav(e, path)}
            >
              <td>
                <a class="row-link user" href={rowHref(path)}>
                  <span
                    class="avatar"
                    style="background: hsl({hueFromString(row.distinct_id)} 50% 45%)"
                  >
                    {initials(row.distinct_id)}
                  </span>
                  <span class="mono uid" title={row.distinct_id}>{row.distinct_id}</span>
                </a>
              </td>
              <td>
                {#if rowTraits.length > 0}
                  <!-- One chip, then a "…" standing in for the rest: three
                       chips wrapped onto two lines and set the row height off
                       every other column. The hover title carries the folded
                       values so nothing is lost. -->
                  <span class="traits">
                    <span class="trait">
                      <span class="tkey">{rowTraits[0].key}</span>
                      <span class="tval mono">{rowTraits[0].value}</span>
                    </span>
                    {#if rowTraits.length > 1}
                      <span
                        class="trait more"
                        title={traitSummary(rowTraits.slice(1))}
                        aria-label={`${rowTraits.length - 1} more trait${rowTraits.length > 2 ? 's' : ''}`}
                      >…</span>
                    {/if}
                  </span>
                {:else}
                  <span class="faint">—</span>
                {/if}
              </td>
              <td class="num">{formatNumber(row.sessions_count)}</td>
              <td class="num">{formatNumber(row.events_count)}</td>
              <td class="num">
                <span class:err={row.errors_count > 0}>{formatNumber(row.errors_count)}</span>
              </td>
              <td class="when"><TimeValue value={row.first_seen} muted /></td>
              <td class="when"><TimeValue value={row.last_seen} muted /></td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    </div>

    <!-- `hasNext` is the client's `limit + 1` over-fetch probe, not an
         inference from the row count: a final page of exactly `LIMIT` rows
         used to offer a Next that led to an empty page. -->
    <Pagination
      offset={list.offset}
      limit={LIMIT}
      count={rows.length}
      {hasNext}
      total={rowCount.total}
      totalIsCapped={rowCount.isCapped}
      onchange={(o) => (list = setOffsetPage(list, o))}
    />
  {/if}

<!-- 503 `release_rollup_pending`: the picked release's numbers are still being
     backfilled. Calm on purpose — nothing failed, and Retry-as-error wording
     would invite a bug report for a job that is simply not finished. -->
{#snippet releasePending(full: boolean)}
  {#if full}
    <EmptyState
      title={t('users.releasePending.title')}
      description={t('users.releasePending.body')}
      icon="timer"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={checkAgain}>
          {t('users.releasePending.checkAgain')}
        </Button>
      {/snippet}
    </EmptyState>
  {:else}
    <EmptyState title={t('users.releasePending.title')} icon="timer" />
  {/if}
{/snippet}

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 28px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  /* Section rhythm. The page runs header -> Audience (tiles + chart) -> People
     (table + pagination). Each section opens with a heading, so the gap ABOVE a
     heading is what separates two sections and must stay clearly larger than
     the gap below it (which only ties a heading to its own content). At the
     previous 24/12 the two were close enough that "Audience" read as floating
     between the blocks rather than belonging to the one under it. */
  .analytics-head,
  .people-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin: 40px 0 16px;
  }
  /* The People heading owns the space above the table, so this wrapper adds no
     bottom margin of its own. That also retires the `.audience:empty` guard
     that used to sit here: it existed to cancel a `margin-bottom` during the
     first paint, before either the tiles or the error Card had landed, and
     there is no longer a margin to cancel. */
  .audience {
    display: flex;
    flex-direction: column;
    gap: 24px;
  }
  .section-hint {
    font-size: 13px;
    margin-top: 2px;
  }
  .section-title {
    font-size: 15px;
    font-weight: 640;
    margin: 0;
  }
  /* The `DateRange` pill well, so it reads as the same family as the window
     control beside it and sits at `--control-h`: 3px inset + 28px pills +
     1px border. There is no shared segmented-control component to reuse. */
  .release-mode {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    min-height: var(--control-h);
    padding: 3px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .mode {
    height: 28px;
    padding: 0 12px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    border-radius: var(--radius-sm);
    white-space: nowrap;
    transition: color 0.13s ease, background 0.13s ease;
  }
  .mode:hover {
    color: var(--text);
  }
  .mode.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .table {
    transition: opacity 0.12s ease;
  }
  .table.loading {
    opacity: 0.55;
  }
  .user {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .avatar {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
    font-size: 10.5px;
    font-weight: 680;
    flex-shrink: 0;
    text-shadow: 0 1px 1px rgba(0, 0, 0, 0.25);
  }
  .uid {
    font-size: 12px;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: inline-block;
    vertical-align: middle;
  }
  /* `nowrap`, not `wrap`: at narrow widths a long value used to push the "…"
     onto a second line, which is the row-height unevenness this column was
     folded to remove. The value truncates instead — `min-width: 0` is what lets
     it, since a flex item's default `min-width: auto` refuses to shrink below
     its content and the overflow would escape the cell rather than ellipsize. */
  .traits {
    display: inline-flex;
    flex-wrap: nowrap;
    gap: 6px;
    max-width: 100%;
    min-width: 0;
  }
  .trait {
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    padding: 2px 9px;
    max-width: 220px;
    min-width: 0;
  }
  /* Reads as "there is more here" rather than as another trait: no key/value
     pair, tighter, and carrying the hover cue. `help` over `pointer` because
     the row's own click opens the person — a pointer here would promise a
     separate action that does not exist. */
  .more {
    padding: 2px 8px;
    /* `--text-muted`, not the `--text-faint` used by `.tkey` beside it: that
       token measures 2.86:1 on this chip in the light theme. A key sitting next
       to its own value can afford that; this glyph is the only thing on screen
       saying more traits exist, so it has to clear AA (5.77:1 light, 6.69:1
       dark). */
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.1;
    cursor: help;
    /* Never the item that gives way when the row is tight — the whole point of
       the chip is that it stays visible to say more exists. */
    flex: none;
  }
  .tkey {
    font-size: 10px;
    font-weight: 640;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .tval {
    font-size: 11.5px;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    color: var(--error);
    font-weight: 620;
  }
  .when {
    font-size: 12.5px;
  }

  @media (max-width: 640px) {
    .uid {
      max-width: 150px;
    }
  }
</style>
