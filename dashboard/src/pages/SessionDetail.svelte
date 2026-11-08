<script lang="ts">
  import { t } from '../lib/i18n';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { formatNumber } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import SegmentedControl from '../lib/components/ui/SegmentedControl.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import Timeline from '../lib/components/Timeline.svelte';
  import TimelineFilters from '../lib/components/TimelineFilters.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import {
    getSessionSummary,
    getSessionTimeline,
    type SessionTimelinePage,
  } from '../lib/api/sessions';
  import { isNormalizedError } from '../lib/api/client';
  import {
    formatDuration,
    durationBetween,
    formatTimestamp,
    relativeTime,
  } from '../lib/utils/format';
  import type { Session, TimelineItem, Transaction } from '../lib/models';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import {
    NO_TIMELINE_FILTER,
    categoryCounts,
    filterTimeline,
    isTimelineFiltered,
    opCounts,
    type TimeMode,
    type TimelineFilter,
  } from '../lib/models/timeline-row';

  interface Props {
    params?: { id?: string };
  }
  let { params }: Props = $props();

  const sessionId = $derived(decodeURIComponent(params?.id ?? ''));

  /**
   * A 404 here is a fact about this session, not a transport failure: the page
   * has its own empty state for it, and it must not travel the error path (which
   * would read "Couldn't load session" and offer a Retry that can only 404
   * again). So it resolves to `null` — a legitimate payload the cache may hold,
   * which also keeps the not-found state paintable without a second round trip.
   * Every other failure still throws and lands in `view.error`.
   *
   * Resolving rather than throwing is also what keeps the 404 subject to
   * CachedView's generation guard: a late 404 for a session the user has already
   * navigated away from is discarded like any other stale response, instead of
   * flipping a page-level flag over the session now on screen.
   */
  async function fetchSession(appId: string, id: string): Promise<Session | null> {
    try {
      return await getSessionSummary(appId, id);
    } catch (err) {
      if (isNormalizedError(err) && err.status === 404) return null;
      throw err;
    }
  }

  // Cached view (lib/stores/cached-view.svelte.ts): a session visited a moment ago
  // paints instantly on return and refreshes behind the rendered page instead of
  // blanking to a spinner. Re-exposed under the names the template already used,
  // so the markup is unchanged apart from Retry now forcing a network hit.
  //
  // `revalidating` now IS surfaced: the page grew a RefreshButton, and a
  // refresh that spun nothing while the payload swapped underneath read as the
  // click having done nothing.
  //
  // TWO views. The session row is one keyed lookup; its timeline is up to 1,500
  // rows plus symbolication. Read as one payload, the header and the stat tiles
  // — all of which come from the row — waited on the timeline. Read apart, the
  // page says what the session IS at once and the timeline card shows a
  // skeleton until its own answer lands.
  const view = new CachedView<Session | null>();
  const timelineView = new CachedView<SessionTimelinePage>();

  const refresher = pageRefresher(async () => {
    await Promise.all([view.reload(), timelineView.reload()]);
  });

  const loading = $derived(view.loading);
  const error = $derived(view.error);
  // "Loaded, and what loaded was nothing." `hasData` is what separates that from
  // "nothing loaded yet", since both leave `s` null.
  const notFound = $derived(view.hasData && view.data === null);

  // `scopeKey` belongs in the key: it carries the selected environment, which the
  // axios interceptor adds to the request but which appears in none of these
  // arguments. Omit it and one environment's session would be served as another's.
  //
  // `force` bypasses the fresh-window short-circuit: the Retry button means "go to
  // the network now", and honouring the cache there makes the control look broken.
  //
  // Started together, settled separately — see the note on the two views above.
  async function load(appId: string, id: string, force = false) {
    await Promise.allSettled([
      view.load(
        viewKey('sessions.detail', appId, sessionStore.scopeKey, id),
        () => fetchSession(appId, id),
        force,
      ),
      timelineView.load(
        viewKey('sessions.detail.timeline', appId, sessionStore.scopeKey, id),
        () => getSessionTimeline(appId, id),
        force,
      ),
    ]);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const id = sessionId;
    // Reset the timeline filter alongside the load. The router reuses this
    // component across `#/sessions/A` → `#/sessions/B`, so without this a
    // filter set on one session would carry into the next and hide rows there
    // — and the op chips it names may not even exist in the new session.
    // Safe inside the effect: nothing here reads `timelineFilter`.
    timelineFilter = NO_TIMELINE_FILTER;
    if (aid && id) void load(aid, id);
  });

  // What the Timeline's trailing offsets read against. Deliberately not
  // persisted: "since the session started" is the right first answer for a
  // session you have just opened, and a remembered delta mode would silently
  // change what every future session's numbers mean.
  let timeMode = $state<TimeMode>('session');

  /**
   * Which lanes of the timeline are on screen. Not persisted, for the reason
   * `timeMode` is not: a remembered filter would silently hide rows on the next
   * session you open, and the ops it names belong to the session it was set on.
   *
   * Replaced wholesale on every change — never mutated. Svelte 5 does not proxy
   * `Set`, so mutating one in place would leave both the list and the chips
   * unchanged.
   */
  let timelineFilter = $state<TimelineFilter>(NO_TIMELINE_FILTER);

  // `?.items`: the cached page is an object since the timeline gained its
  // archive flag; anything else cached under the key reads as empty.
  const timeline = $derived<TimelineItem[]>(timelineView.data?.items ?? []);
  const archiveIncomplete = $derived(timelineView.data?.archiveIncomplete === true);
  // The card has rows to show only once BOTH have landed: the timeline for the
  // rows, the session for the instant their offsets are measured from.
  const timelineReady = $derived(timelineView.hasData && view.data != null);
  // Counts read the FULL timeline: a chip whose number moved when you toggled a
  // different chip could not be read as "how many of these this session has".
  const timelineCounts = $derived(categoryCounts(timeline));
  const timelineOps = $derived(opCounts(timeline));
  const visibleTimeline = $derived(filterTimeline(timeline, timelineFilter));
  const timelineFiltered = $derived(isTimelineFiltered(timelineFilter));

  const s = $derived(view.data ?? null);
  const durationMs = $derived(s ? durationBetween(s.started_at, s.last_event_at) : 0);
  const hasContext = $derived(
    !!s && !!s.context && typeof s.context === 'object' && Object.keys(s.context).length > 0,
  );

  let sliceStack = $state<Transaction[]>([]);
  let sliceStartTime = $state<'occurred_at' | 'received_at'>('occurred_at');

  const slicedTimeline = $derived.by(() => {
    if (sliceStack.length === 0) return [];
    const currentSlice = sliceStack[sliceStack.length - 1];
    
    const startMs = new Date(sliceStartTime === 'occurred_at' ? currentSlice.occurred_at : currentSlice.received_at).getTime();
    const endMs = currentSlice.finished_at 
      ? new Date(currentSlice.finished_at).getTime()
      : new Date(currentSlice.occurred_at).getTime() + currentSlice.duration_ms;

    return timeline.filter(item => {
      const itemMs = new Date(item.at).getTime();
      const inRange = itemMs >= startMs && itemMs <= endMs;
      if (!inRange) return false;
      if (item.kind === 'transaction' && item.transaction.id === currentSlice.id) return false;
      return true;
    });
  });

  const sliceOptions = $derived([
    { value: 'occurred_at', label: t('session.slice.occurredAt') },
    { value: 'received_at', label: t('session.slice.receivedAt') },
  ]);

  function closeSliceModal() {
    sliceStack = [];
  }

  function pushSlice(tx: Transaction) {
    sliceStack = [...sliceStack, tx];
  }

  function downloadSessionJson() {
    // Not before the timeline has landed: an export of the session with an
    // empty `timeline` would read as a session in which nothing happened.
    if (!s || !timelineView.hasData) return;
    const exportData = {
      session: s,
      timeline,
    };
    const blob = new Blob([JSON.stringify(exportData, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `session-${s.session_id}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }
</script>

  <PageHeader
    title={s?.session_id ?? sessionId}
    titleClass="mono"
    back={{ label: t('nav.sessions'), href: '#/sessions' }}
  >
    {#snippet meta()}
      {#if s}
        <CopyButton value={s.session_id} size="sm" />
        {#if s.distinct_id}
          <a class="meta-link mono" href={`#/persons/${encodeURIComponent(s.distinct_id)}`}>
            <Icon name="user" size={13} />{s.distinct_id}
          </a>
        {:else}
          <span class="meta-link"><Icon name="user" size={13} />{t('session.meta.anonymous')}</span>
        {/if}
        {#if s.device_key}
          <a class="meta-link mono" href={`#/devices/${encodeURIComponent(s.device_key)}`}>
            <Icon name="monitor" size={13} />{s.device_key}
          </a>
        {/if}
        {#if s.release}<Badge tone="neutral" size="sm">{t('session.meta.release', { release: s.release })}</Badge>{/if}
        {#if s.environment_id}
          <span class="meta-link mono">{t('session.meta.env', { env: s.environment_id })}</span>
        {/if}
      {/if}
    {/snippet}
    {#snippet freshness()}
      {#if s}
        <Freshness
          fetchedAt={view.fetchedAt}
          revalidating={view.revalidating || timelineView.revalidating}
        />
      {/if}
    {/snippet}
    {#snippet refresh()}
      <RefreshButton
        onclick={refresher.run}
        loading={refresher.busy || view.revalidating || timelineView.revalidating}
      />
    {/snippet}
  </PageHeader>

  <!-- The session ROW decides whether there is a page at all (not found, or a
       failed read); the timeline is a section of a page that exists, and waits
       and fails inside its own card. -->
  {#if notFound}
    <EmptyState
      title={t('session.notFound.title')}
      description={t('session.notFound.body')}
      icon="inbox"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/sessions')}>{t('session.backToList')}</Button>
      {/snippet}
    </EmptyState>
  {:else if !loading && error}
    <ErrorState
      title={t('session.error.load')}
      error={view}
      onretry={() => sessionStore.currentAppId && load(sessionStore.currentAppId, sessionId, true)}
    />
  {:else}
    {#if !s}
      <Skeleton rows={1} height="56px" />
    {:else}

    <StatTiles min={160}>
      <StatTile label={t('explore.column.duration')} value={formatDuration(durationMs)} />
      <StatTile label={t('explore.column.events')} value={formatNumber(s.events_count)} />
      <StatTile
        label={t('explore.column.errors')}
        value={formatNumber(s.errors_count)}
        tone={s.errors_count > 0 ? 'error' : 'neutral'}
      />
      <!-- A tile's value is plain text, so it cannot hold the TimeValue toggle;
           it shows both halves of it instead — relative, with the exact
           instant under it in TimeValue's own absolute format. -->
      <StatTile
        label={t('explore.column.started')}
        value={relativeTime(s.started_at)}
        sub={formatTimestamp(s.started_at)}
      />
    </StatTiles>
    {/if}

    <div class="detail-layout">
      <div class="col-main">
        <Card title={t('session.card.timeline')}>
          {#snippet actions()}
            <Button
              variant="ghost"
              size="sm"
              title={t('session.downloadTitle')}
              disabled={!timelineReady}
              onclick={downloadSessionJson}
            >
              <Icon name="download" size={14} />
              {t('explore.downloadJson')}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              title={timeMode === 'delta'
                ? t('session.timeMode.deltaTitle')
                : t('session.timeMode.startTitle')}
              onclick={() => (timeMode = timeMode === 'delta' ? 'session' : 'delta')}
            >
              <Icon name="clock" size={14} />
              {timeMode === 'delta' ? t('timeline.mode.sincePrevious') : t('session.timeMode.sinceStart')}
            </Button>
          {/snippet}
          <!-- Hidden for an empty session: with every count at zero the strip
               is four disabled chips over a timeline that already says it has
               nothing in it. -->
          <ViewSection view={timelineView} rows={8} waiting={!s}>
            {#if archiveIncomplete}
              <p class="archive-note" role="status">{t('timeline.archiveIncomplete')}</p>
            {/if}
            {#if s}
              {#if timeline.length > 0}
                <TimelineFilters
                  counts={timelineCounts}
                  ops={timelineOps}
                  filter={timelineFilter}
                  onchange={(next) => (timelineFilter = next)}
                />
              {/if}
              <Timeline
                items={visibleTimeline}
                startedAt={s.started_at}
                {timeMode}
                onslice={pushSlice}
                emptyLabel={timelineFiltered
                  ? t('common.empty.match', { things: t('timeline.things.entries') })
                  : undefined}
              />
            {/if}
          </ViewSection>
        </Card>
      </div>
      <aside class="col-side">
        <Card title={t('session.card.context')}>
          {#if !s}
            <Skeleton rows={4} />
          {:else if hasContext}
            <div class="ctx"><JsonTree value={s.context} expandTo={1} /></div>
          {:else}
            <EmptyState size="section" title={t('session.empty.context')} />
          {/if}
        </Card>
      </aside>
    </div>
  {/if}

  <Modal open={sliceStack.length > 0} onclose={closeSliceModal} size="xl" title={t('session.inBetweenTransaction')}>
    {#if sliceStack.length > 0}
      <div class="slice-header">
        <div class="slice-breadcrumbs">
          {#each sliceStack as tx, i}
            <button class="breadcrumb-btn" onclick={() => (sliceStack = sliceStack.slice(0, i + 1))}>
              {tx.name || tx.op || t('timeline.kind.transaction')}
            </button>
            {#if i < sliceStack.length - 1}
              <Icon name="chevron-right" size={12} />
            {/if}
          {/each}
        </div>
        <SegmentedControl
          size="sm"
          ariaLabel={t('session.slice.measureFrom')}
          options={sliceOptions}
          value={sliceStartTime}
          onchange={(v) => (sliceStartTime = v as 'occurred_at' | 'received_at')}
        />
      </div>
      <div class="slice-timeline">
        <!-- The slice deliberately ignores the category filter — it answers
             "what happened inside this span", which a filter set for the page
             behind the modal has no bearing on. It does need its own empty
             text: a span with nothing between its ends is not a session with
             no activity, and the default would say so. -->
        <Timeline
          items={slicedTimeline}
          startedAt={sliceStack[sliceStack.length - 1][sliceStartTime]}
          timeMode="delta"
          onslice={pushSlice}
          emptyLabel={t('session.slice.empty')}
        />
      </div>
    {/if}
  </Modal>

<style>
  .archive-note {
    margin: 0 0 12px;
    font-size: var(--text-xs);
    color: var(--text-faint);
  }
  /* Links in the header's meta line: plain muted text with an icon, not pills. */
  .meta-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--text-muted);
  }
  a.meta-link:hover {
    color: var(--text);
    text-decoration: underline;
  }

  /* The rail layout itself is the shared `.detail-layout` (app.css). */
  .detail-layout {
    margin-top: var(--page-gap);
  }
  .col-main {
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }
  .ctx {
    overflow-x: auto;
  }
  .slice-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 16px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }
  .slice-breadcrumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    font-size: var(--text-sm);
  }
  .breadcrumb-btn {
    background: none;
    border: none;
    color: var(--primary);
    cursor: pointer;
    padding: 2px 4px;
    border-radius: var(--radius-sm);
  }
  .breadcrumb-btn:hover {
    background: var(--surface-3);
  }
</style>
