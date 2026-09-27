<script lang="ts">
  import { t } from '../lib/i18n';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { formatNumber } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
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
  import { getSessionSummary, getSessionTimeline } from '../lib/api/sessions';
  import { isNormalizedError } from '../lib/api/client';
  import { formatDateTime, formatDuration, durationBetween } from '../lib/utils/format';
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
  const timelineView = new CachedView<TimelineItem[]>();

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

  const timeline = $derived(timelineView.data ?? []);
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

  <button class="back" onclick={() => push('/sessions')}><Icon name="arrow-left" size={14} /> {t('explore.column.sessions')}</button>

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
    <EmptyState title={t('session.error.load')} description={error} icon="triangle-alert">
      {#snippet action()}
        <Button
          variant="secondary"
          onclick={() => sessionStore.currentAppId && load(sessionStore.currentAppId, sessionId, true)}
        >
          {t('common.retry')}
        </Button>
      {/snippet}
    </EmptyState>
  {:else}
    {#if !s}
      <header class="detail-head"><Skeleton rows={2} height="18px" /></header>
      <Skeleton rows={1} height="64px" />
    {:else}
    <header class="detail-head">
      <div class="refresh-slot">
        <Freshness
          fetchedAt={view.fetchedAt}
          revalidating={view.revalidating || timelineView.revalidating}
        />
        <RefreshButton
          onclick={refresher.run}
          loading={refresher.busy || view.revalidating || timelineView.revalidating}
        />
      </div>
      <div class="id-row">
        <h1 class="session-id mono">{s.session_id}</h1>
        <CopyButton value={s.session_id} size="sm" />
      </div>
      <div class="meta-row">
        {#if s.distinct_id}
          <a class="meta-link mono" href={`#/persons/${encodeURIComponent(s.distinct_id)}`}>
            <Icon name="user" size={14} />{s.distinct_id}
          </a>
        {:else}
          <span class="meta-static muted"><Icon name="user" size={14} />anonymous</span>
        {/if}
        {#if s.device_key}
          <a class="meta-link mono" href={`#/devices/${encodeURIComponent(s.device_key)}`}>
            <Icon name="monitor" size={14} />{s.device_key}
          </a>
        {/if}
        {#if s.release}<Badge tone="neutral" size="sm">release {s.release}</Badge>{/if}
        {#if s.environment_id}
          <span class="meta-static faint mono">env {s.environment_id}</span>
        {/if}
      </div>
    </header>

    <StatTiles min={160}>
      <StatTile label={t('explore.column.duration')} value={formatDuration(durationMs)} />
      <StatTile label={t('explore.column.events')} value={formatNumber(s.events_count)} />
      <StatTile
        label={t('explore.column.errors')}
        value={formatNumber(s.errors_count)}
        tone={s.errors_count > 0 ? 'error' : 'neutral'}
      />
      <StatTile label={t('explore.column.started')} value={formatDateTime(s.started_at)} />
    </StatTiles>
    {/if}

    <div class="grid">
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
                ? 'Showing time since the previous entry — click to measure from the session start'
                : 'Showing time since the session started — click to measure from the previous entry'}
              onclick={() => (timeMode = timeMode === 'delta' ? 'session' : 'delta')}
            >
              <Icon name="clock" size={14} />
              {timeMode === 'delta' ? 'Since previous' : 'Since start'}
            </Button>
          {/snippet}
          <!-- Hidden for an empty session: with every count at zero the strip
               is four disabled chips over a timeline that already says it has
               nothing in it. -->
          <ViewSection view={timelineView} rows={8} waiting={!s}>
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
                  ? 'No entries match the selected filters.'
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
            <p class="muted empty-ctx">{t('session.noContext')}</p>
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
              {tx.name || tx.op || 'transaction'}
            </button>
            {#if i < sliceStack.length - 1}
              <Icon name="chevron-right" size={12} />
            {/if}
          {/each}
        </div>
        <div class="slice-toggles">
          <Button
            size="sm"
            variant={sliceStartTime === 'occurred_at' ? 'primary' : 'secondary'}
            onclick={() => (sliceStartTime = 'occurred_at')}
          >
            occurred_at
          </Button>
          <Button
            size="sm"
            variant={sliceStartTime === 'received_at' ? 'primary' : 'secondary'}
            onclick={() => (sliceStartTime = 'received_at')}
          >
            received_at
          </Button>
        </div>
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
          emptyLabel="Nothing else was recorded while this transaction was open."
        />
      </div>
    {/if}
  </Modal>

<style>
  .refresh-slot {
    float: right;
    margin-inline-start: 12px;
  }

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
  .detail-head {
    margin-bottom: 20px;
  }
  .id-row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .session-id {
    font-size: 20px;
    font-weight: 640;
    word-break: break-all;
  }
  .meta-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
    flex-wrap: wrap;
  }
  .meta-link,
  .meta-static {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 4px 10px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--border);
    background: var(--surface-2);
  }
  .meta-link {
    color: var(--text-muted);
    text-decoration: none;
    transition: color 0.12s ease, border-color 0.12s ease;
  }
  .meta-link:hover {
    color: var(--primary);
    border-color: var(--primary-border);
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 340px;
    gap: 18px;
    align-items: start;
    margin-top: 20px;
  }
  .col-main {
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .ctx {
    overflow-x: auto;
  }
  .empty-ctx {
    font-size: 13px;
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
    font-size: 13px;
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
  .slice-toggles {
    display: flex;
    gap: 8px;
  }

  @media (max-width: 960px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
