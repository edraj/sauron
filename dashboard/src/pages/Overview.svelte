<script lang="ts">
  import { t, localeStore, intlTag } from '../lib/i18n';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import {
    formatAbsolute,
    rangeKey,
    spanDays,
    type DateRangeValue,
  } from '../lib/models/date-range';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import StoreSection from '../lib/components/StoreSection.svelte';
  import { shouldShowStoreSection } from '../lib/components/stores';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import {
    getOverviewTotals,
    getOverviewSeries,
    getOverviewTopIssues,
    getOverviewTopEvents,
    getActiveUsersSeries,
    refreshOverview,
  } from '../lib/api/overview';
  import type {
    ActiveUsersSeries,
    OverviewEnvelope,
    OverviewSectionName,
    OverviewTotalsSection,
    OverviewSeriesSection,
  } from '../lib/api/overview';
  import { openOverviewStream } from '../lib/api/overview-stream';
  import { compactNumber, formatPercent, formatTime, relativeTime } from '../lib/utils/format';
  import type { Issue, TopEvent } from '../lib/models';

  const RANGES = [
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  // The SHARED selection, falling back to this page's own 30 days until the
  // user has chosen one. See `stores/range.svelte.ts`.
  let range = $state<DateRangeValue>(rangeStore.effective(30));
  /** The cache-key component. Never the resolved instants — those move. */
  const rkey = $derived(rangeKey(range));
  /**
   * The window, in words. A custom range reads as its own label ("July 2026")
   * rather than "the last 31 days", which would be true but is not what the
   * user asked for.
   */
  const rangeCaption = $derived(
    range.kind === 'last'
      ? `the last ${spanDays(range)} days`
      : formatAbsolute(range, intlTag(localeStore.locale)),
  );

  /**
   * The current app, for the store-section gate.
   *
   * Read from `sessionStore` rather than fetched: the designation travels on
   * the app record the switcher already holds, so this costs no request and
   * updates the moment App settings changes it.
   */
  const storeApp = $derived(sessionStore.currentApp);

  // FOUR independent views, not one.
  //
  // `/overview` runs five aggregates sequentially on one server connection and
  // returns nothing until the last finishes, so its latency is their sum —
  // measured at ~165 ms + ~160 ms + ~180 ms + series on a 210k-event app, and it
  // scales with the data. The whole page waited on that.
  //
  // Each section now fetches its own endpoint, so the browser issues them in
  // parallel (wall clock is the MAX, not the sum) and each card paints the moment
  // its own answer lands. Sections are also cached and revalidated separately,
  // which is what lets a return visit show the KPI tiles instantly while only the
  // slow half refreshes.
  // Each view now holds the ENVELOPE, not the payload.
  //
  // These endpoints no longer run their aggregate on the request path — they
  // answer from a server-side cache in milliseconds and enqueue a background
  // recompute, with the result pushed over SSE. `data: null` is therefore a
  // normal 200 meaning "computing", which is what the whole design buys: the
  // slowest section was past the server's 30s timeout and shed as a 503, so the
  // KPI tiles rendered as an error rather than slowly.
  //
  // Caching the envelope rather than the payload is deliberate. `computed_at`
  // is what the header reports, and it has to survive a navigation with the
  // value it describes — split them and the page comes back showing yesterday's
  // numbers under a timestamp from this visit.
  const totalsView = new CachedView<OverviewEnvelope<OverviewTotalsSection>>();
  const seriesView = new CachedView<OverviewEnvelope<OverviewSeriesSection>>();
  const issuesView = new CachedView<OverviewEnvelope<Issue[]>>();
  const eventsView = new CachedView<OverviewEnvelope<TopEvent[]>>();
  const activeUsersView = new CachedView<OverviewEnvelope<ActiveUsersSeries>>();

  // `?? null` on the INNER data too: an envelope in the `computing` state has a
  // null payload, and every card below already renders its skeleton for a null.
  // That is the cold-start UX with no template change — a section shows its
  // skeleton until its own push lands, rather than the page waiting on the
  // slowest one.
  const totals = $derived(totalsView.data?.data ?? null);
  const series = $derived(seriesView.data?.data ?? null);
  const topIssues = $derived(issuesView.data?.data ?? null);
  const topEvents = $derived(eventsView.data?.data ?? null);
  const activeUsers = $derived(activeUsersView.data?.data ?? null);

  /**
   * Wire section name -> the view that holds it and its cache-key prefix.
   *
   * Keyed by the SERVER's name for each section, which is the same string used
   * in the cache key and the SSE event, so the three cannot drift apart. The
   * prefixes are the ones `load()` already passes to `viewKey`; a mismatch here
   * would write pushes into a key nothing reads, and the only symptom would be
   * that live updates silently never appear.
   */
  const SECTION_VIEWS: Record<
    OverviewSectionName,
    { view: CachedView<OverviewEnvelope<unknown>>; key: string }
  > = {
    totals: { view: totalsView as CachedView<OverviewEnvelope<unknown>>, key: 'overview.totals' },
    series: { view: seriesView as CachedView<OverviewEnvelope<unknown>>, key: 'overview.series' },
    'top-issues': {
      view: issuesView as CachedView<OverviewEnvelope<unknown>>,
      key: 'overview.topIssues',
    },
    'top-events': {
      view: eventsView as CachedView<OverviewEnvelope<unknown>>,
      key: 'overview.topEvents',
    },
    'active-users': {
      view: activeUsersView as CachedView<OverviewEnvelope<unknown>>,
      key: 'overview.activeUsers',
    },
  };

  /**
   * When the page's numbers were computed — the OLDEST across sections.
   *
   * Oldest, not newest, because this one label speaks for the whole page: the
   * sections refresh independently and drift apart, and reporting the newest
   * would advertise the page as fresher than its stalest tile actually is.
   *
   * `null` while nothing has landed yet, which hides the label rather than
   * showing "just now" for numbers that do not exist.
   */
  const computedAt = $derived.by(() => {
    const stamps = [totalsView, seriesView, issuesView, eventsView, activeUsersView]
      .map((v) => v.data?.computed_at)
      .filter((s): s is string => !!s)
      .map((s) => new Date(s).getTime())
      .filter((ms) => Number.isFinite(ms));
    return stamps.length > 0 ? new Date(Math.min(...stamps)) : null;
  });

  /**
   * True while any section is still waiting on its first server-side compute.
   *
   * A 403 on top-issues cannot reach this: that view holds an error and no
   * envelope, so its `state` is undefined rather than `computing`. No special
   * case needed — worth stating, because the obvious defensive `!issuesForbidden`
   * guard would reference a binding declared further down.
   */
  const computing = $derived(
    [totalsView, seriesView, issuesView, eventsView, activeUsersView].some(
      (v) => v.data?.state === 'computing',
    ),
  );

  /**
   * `{ day, count }` from the API remapped to the `{ bucket, count }` that
   * `TimeSeriesChart` takes. Two different wire shapes for the same idea, so the
   * translation is explicit here rather than hidden behind a cast that would
   * silently plot `undefined`.
   */
  const activeUsersChart = $derived(
    (activeUsers?.series ?? []).map((p) => ({ bucket: p.day, count: p.count })),
  );

  // `revalidating` is the OR across sections: the one refresh button speaks for
  // the whole page, and a spinner that stopped while a section was still fetching
  // would claim the page was settled when it was not.
  const revalidating = $derived(
    totalsView.revalidating ||
      seriesView.revalidating ||
      issuesView.revalidating ||
      eventsView.revalidating ||
      activeUsersView.revalidating,
  );

  /**
   * Top issues additionally needs `issue:read`, and the endpoint answers 403
   * rather than an empty list so the card can be HIDDEN instead of showing a
   * reassuring "No issues" to someone who simply cannot see them.
   *
   * Reads the status, not the prose. This used to be
   * `/forbidden|permission|403/i.test(issuesView.error)`, which is wrong in both
   * directions: an unrelated failure whose message merely contains the word
   * "permission" would hide the card, and a 403 phrased without any of those
   * words would leave it showing an error. `CachedView.errorStatus` now carries
   * the code, so the check can be exact.
   */
  const issuesForbidden = $derived(issuesView.errorStatus === 403);

  let refreshing = $state(false);

  /**
   * `force` bypasses the fresh-window short-circuit: Refresh and Retry both mean
   * "go to the network now".
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's overview is served as another's.
   */
  async function load(appId: string, win: DateRangeValue, force = false) {
    const key = rangeKey(win);
    const scope = sessionStore.scopeKey;
    // Started together and NOT awaited in sequence: awaiting them one after
    // another here would rebuild exactly the sum-of-latencies the split exists to
    // remove. `allSettled`, not `all` — each section reports its own failure
    // through its own view, and one 403 or timeout must not abort the others.
    await Promise.allSettled([
      totalsView.load(
        viewKey('overview.totals', appId, scope, key),
        () => getOverviewTotals(appId, win),
        force,
      ),
      seriesView.load(
        viewKey('overview.series', appId, scope, key),
        () => getOverviewSeries(appId, win),
        force,
      ),
      issuesView.load(
        viewKey('overview.topIssues', appId, scope, key),
        () => getOverviewTopIssues(appId, win),
        force,
      ),
      eventsView.load(
        viewKey('overview.topEvents', appId, scope, key),
        () => getOverviewTopEvents(appId, win),
        force,
      ),
      activeUsersView.load(
        viewKey('overview.activeUsers', appId, scope, key),
        () => getActiveUsersSeries(appId, win),
        force,
      ),
    ]);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const win = range;
    if (aid) void load(aid, win);
  });

  /**
   * The push half: whatever the server finishes recomputing lands here.
   *
   * Opened in its OWN effect, not folded into the load effect above, so the
   * stream's lifetime is tied to the scope it belongs to and torn down by
   * Svelte on any change. Folding them would leak a stream per environment
   * switch, each still writing into the shared view cache — one of which would
   * be writing another environment's numbers.
   *
   * The server sends a snapshot of every section on connect, so this converges
   * regardless of whether it opens before or after `load()` finishes. That
   * closes the race the HTTP-only version cannot: fetch returns `computing`,
   * the recompute finishes before the stream is open, and the push is fanned
   * out to nobody — leaving a permanent skeleton over a value that is sitting
   * in Redis.
   */
  $effect(() => {
    const aid = sessionStore.currentAppId;
    const scope = sessionStore.scopeKey;
    const win = range;
    const key = rkey;
    if (!aid) return;

    const handle = openOverviewStream(aid, win, {
      onSection: (frame) => {
        const target = SECTION_VIEWS[frame.section];
        if (!target) return; // unknown section from a newer server — ignore
        const vk = viewKey(target.key, aid, scope, key);
        // `adopt` writes through to the view cache, so a pushed value survives
        // navigating away and back. The second argument is the key the page is
        // CURRENTLY showing: identical here, but passing it explicitly is what
        // stops a late frame from a previous scope painting over the new one.
        target.view.adopt(vk, vk, frame as never);
      },
      // A dropped stream is not an error the user can act on — the sections
      // still hold their last value and the next navigation re-reads them over
      // plain HTTP. Logged, not surfaced.
      onError: (err) => console.warn('overview stream closed', err),
    });
    return () => handle.close();
  });

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid, range, true);
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      // Two calls, and both are needed.
      //
      // `refreshOverview` tells the SERVER to recompute all five sections
      // ignoring its 1h freshness window; it returns 202 as soon as the work is
      // enqueued, because the aggregates take seconds to tens of seconds and
      // waiting for them is the failure this design removes. The results arrive
      // on the stream.
      //
      // `load(force)` then re-reads the sections so the page immediately
      // reflects the new `stale`/`computing` states — without it, clicking
      // Refresh would appear to do nothing at all until the first push landed.
      await refreshOverview(aid, range);
      await load(aid, range, true);
    } finally {
      refreshing = false;
    }
  }

  // A null rate means "not measurable", so the subtitle must say why rather
  // than print "0 crashed" — which reads as a real, perfect number and is the
  // exact confident-lie this nullability exists to prevent.
  //
  // Null has TWO causes and they need different copy. Blaming the SDK for an
  // empty range is its own small confident lie: verified in a live drive, an
  // app with no sessions in the window read "No crash data from this SDK",
  // which sends someone to check their SDK setup over a date filter.
  const crashFreeSub = $derived.by(() => {
    if (totals == null) return undefined;
    if (totals.crash_free_sessions != null) {
      return `${compactNumber(totals.totals.crashed_sessions)} crashed`;
    }
    return totals.totals.sessions === 0
      ? t('overview.stat.crashFree.noSessions')
      : t('overview.stat.crashFree.noSignal');
  });

  // Tone helpers — severity-driven coloring for the KPI row.
  const crashFreeTone = $derived.by(() => {
    const v = totals?.crash_free_sessions;
    if (v == null) return 'neutral';
    if (v >= 0.99) return 'success';
    if (v >= 0.95) return 'warning';
    return 'error';
  });

  const errorRateTone = $derived.by(() => {
    const v = totals?.error_rate;
    if (v == null) return 'neutral';
    if (v >= 0.05) return 'error';
    if (v >= 0.01) return 'warning';
    return 'success';
  });

  const newUserShare = $derived.by(() => {
    const sums = totals?.totals;
    if (!sums || sums.users <= 0) return null;
    return sums.new_users / sums.users;
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">{t('overview.title')}</h1>
      <p class="muted sub">
        Health and activity at a glance for {rangeCaption}.
        <!--
          When these numbers were computed, not when they were fetched. The
          server caches each section for up to an hour and recomputes in the
          background, so a page that painted instantly can still be showing
          40-minute-old numbers — saying so is the contract, not a detail.

          The absolute time is the label and the relative one is the qualifier:
          "42m ago" alone forces the reader to do arithmetic to know whether it
          crosses something they care about, and it silently goes wrong if the
          tab is left open.
        -->
        {#if computedAt}
          <span class="stamp" title={computedAt.toISOString()}>
            · Updated {formatTime(computedAt)} <span class="muted">({relativeTime(computedAt)})</span>
          </span>
        {:else if computing}
          <span class="stamp">· Computing…</span>
        {/if}
      </p>
    </div>
    <div class="controls">
      <DateRange
        value={range}
        onchange={(v) => {
          range = v;
          rangeStore.set(v);
        }}
        ranges={RANGES}
      />
      <!--
        Spins for a background revalidate too, not just an explicit click: that
        spinner IS the "showing cached data, fetching fresh" hint.
      -->
      <RefreshButton
        onclick={refresh}
        loading={refreshing || revalidating}
        title={revalidating ? 'Refreshing…' : 'Refresh'}
      />
    </div>
  </div>

  <!--
    No page-wide {#if loading} gate any more. That single condition is what made
    the whole page wait on the slowest query: every card was inside it. Each
    section now decides for itself whether it has data, is still arriving, or
    failed — so the KPI tiles appear while the charts are still loading, and a
    failing section degrades to its own message instead of blanking the page.
  -->
  {#if totals}
    <StatTiles min={150}>
      <StatTile label={t('overview.stat.events')} value={compactNumber(totals.totals.events)} tone="primary" />
      <StatTile
        label={t('overview.stat.errors')}
        value={compactNumber(totals.totals.errors)}
        tone={totals.totals.errors > 0 ? 'error' : 'neutral'}
        sub={`${formatPercent(totals.error_rate)} error rate`}
      />
      <StatTile label={t('overview.stat.sessions')} value={compactNumber(totals.totals.sessions)} />
      <StatTile label={t('overview.stat.users')} value={compactNumber(totals.totals.users)} />
      <StatTile
        label={t('overview.stat.newUsers')}
        value={compactNumber(totals.totals.new_users)}
        sub={newUserShare != null ? `${formatPercent(newUserShare)} of users` : undefined}
      />
      <StatTile
        wide
        label={t('overview.stat.crashFree')}
        value={formatPercent(totals.crash_free_sessions)}
        tone={crashFreeTone}
        sub={crashFreeSub}
      />
      <StatTile
        label={t('perf.stat.errorRate')}
        value={formatPercent(totals.error_rate)}
        tone={errorRateTone}
        sub="errors / events"
      />
    </StatTiles>
  {:else if totalsView.error}
    <Card>
      <EmptyState title={t('overview.error.totals')} description={totalsView.error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>{t('common.retry')}</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <!-- Tile-height rows, so the KPI strip does not jump when it fills in. -->
    <Card><Skeleton rows={2} height="34px" label={t('overview.loading.totals')} /></Card>
  {/if}

  <div class="grid">
    <div class="col">
      <Card title={t('overview.card.eventVolume')}>
        {#if series}
          <TimeSeriesChart
            data={series.events_series}
            height={220}
            color="var(--primary)"
            emptyLabel="No events in this range"
          />
        {:else if seriesView.error}
          <EmptyState title={t('overview.error.chart')} description={seriesView.error} icon="triangle-alert" />
        {:else}
          <Skeleton rows={1} height="220px" label={t('overview.loading.eventVolume')} />
        {/if}
      </Card>
      <Card title={t('overview.card.errorsOverTime')}>
        {#if series}
          <TimeSeriesChart
            data={series.errors_series}
            height={180}
            color="var(--error)"
            emptyLabel="No errors in this range — nice."
          />
        {:else if seriesView.error}
          <EmptyState title={t('overview.error.chart')} description={seriesView.error} icon="triangle-alert" />
        {:else}
          <Skeleton rows={1} height="180px" label={t('overview.loading.errorsOverTime')} />
        {/if}
      </Card>
      <Card title={t('overview.card.activeUsers')}>
        {#if activeUsers}
          <TimeSeriesChart
            data={activeUsersChart}
            height={180}
            color="var(--success, var(--primary))"
            emptyLabel="No identified activity in this range"
          />
          {#if activeUsers.partial_days.length > 0}
            <!--
              Named, not silently dropped. A day whose distinct count cannot be
              computed exactly is left OUT of the series, and an unexplained gap
              in a chart reads as an outage. Empty in the default day-granular
              configuration — see `tier_read::active_users_by_day`.
            -->
            <p class="partial">
              {activeUsers.partial_days.length} day{activeUsers.partial_days.length > 1 ? 's' : ''}
              omitted: {activeUsers.partial_days[0].reason}
            </p>
          {/if}
        {:else if activeUsersView.error}
          <EmptyState
            title={t('overview.error.activeUsers')}
            description={activeUsersView.error}
            icon="triangle-alert"
          />
        {:else}
          <Skeleton rows={1} height="180px" label={t('overview.loading.activeUsers')} />
        {/if}
      </Card>
    </div>

    <div class="col">
      <!--
        Hidden entirely, not shown empty, when the caller lacks `issue:read`: an
        empty "No issues" card would read as good news rather than as data the
        viewer is not permitted to see.
      -->
      {#if !issuesForbidden}
        <Card title={t('overview.card.topIssues')} padding="sm">
          {#if topIssues}
            {#if topIssues.length === 0}
              <EmptyState
                title={t('overview.empty.issues')}
                description={t('overview.empty.issuesBody')}
                icon="check"
              />
            {:else}
              <div class="issues">
                {#each topIssues as issue (issue.id)}
                  <a class="issue-row" href={`#/issues/${issue.id}`}>
                    <span class="issue-title truncate">{issue.title}</span>
                    <LevelBadge level={issue.level} size="sm" />
                    <span class="issue-count mono" title={t('overview.timesSeen')}>
                      {compactNumber(issue.times_seen)}
                    </span>
                  </a>
                {/each}
              </div>
            {/if}
          {:else if issuesView.error}
            <EmptyState title={t('overview.error.issues')} description={issuesView.error} icon="triangle-alert" />
          {:else}
            <Skeleton rows={5} label={t('overview.loading.topIssues')} />
          {/if}
        </Card>
      {/if}

      <Card title={t('overview.card.topEvents')}>
        {#if topEvents}
          {#if topEvents.length === 0}
            <EmptyState
              title={t('events.empty.none')}
              description={t('events.empty.body')}
              icon="chart-column"
            />
          {:else}
            <BarList items={topEvents} />
          {/if}
        {:else if eventsView.error}
          <EmptyState title={t('events.error.load')} description={eventsView.error} icon="triangle-alert" />
        {:else}
          <Skeleton rows={5} label={t('overview.loading.topEvents')} />
        {/if}
      </Card>
    </div>

    <!--
      Store installs, shown only in the environment designated as the store
      build. `StoreSection` owns its own CachedView rather than joining the
      `Promise.allSettled` batch above, so a store-API failure cannot abort the
      other five sections.
    -->
    {#if storeApp && shouldShowStoreSection(storeApp, sessionStore.currentEnvId)}
      <div class="store-row">
        <StoreSection appId={storeApp.id} {range} />
      </div>
    {/if}
  </div>
</AppShell>

<style>
  /* Reason text under the active-users chart. Muted: it explains an absence, it
     is not itself a warning. */
  .partial {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--text-muted, #888);
  }
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  /*
    `white-space: nowrap` so the timestamp and its relative qualifier never
    break across lines — split over two lines they read as two separate facts.
    The subtitle itself still wraps, at the space before the leading "·".
  */
  .stamp {
    white-space: nowrap;
  }
  .grid {
    display: grid;
    grid-template-columns: 1.5fr 1fr;
    gap: 18px;
    margin-top: 18px;
    align-items: start;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }
  .issues {
    display: flex;
    flex-direction: column;
  }
  .issue-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 8px;
    border-radius: var(--radius-sm);
    text-decoration: none;
    color: inherit;
    transition: background 0.12s ease;
  }
  .issue-row:hover {
    background: var(--surface-2);
  }
  .issue-row + .issue-row {
    border-top: 1px solid var(--border);
  }
  .issue-title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    color: var(--text);
  }
  .issue-count {
    flex-shrink: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
