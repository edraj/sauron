<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { getPersonSummary, getPersonTimeline, type PersonTimeline } from '../lib/api/persons';
  import { initials } from '../lib/utils/format';
  import {
    formatOffset,
    personJsonFilename,
    personOffsetMs,
    type PersonTimeMode,
  } from '../lib/models/person-timeline';
  import type { AnalyticsEvent, ErrorEvent, PersonRow } from '../lib/models';
  import Freshness from '../lib/components/ui/Freshness.svelte';

  interface Props {
    params?: { distinctId?: string };
  }
  let { params }: Props = $props();

  // How many events/errors the profile pulls. In the cache key as well as the
  // request: two pages asking for different depths are different payloads.
  const LIMIT = 100;

  const distinctId = $derived(decodeURIComponent(params?.distinctId ?? ''));

  // Cached views (lib/stores/cached-view.svelte.ts): a profile you just looked
  // at paints instantly on return and refreshes behind the render.
  //
  // TWO of them. The profile row is one lookup; the timeline is two capped
  // scans of the event tables. The identity header needs neither — it renders
  // from the id in the URL — so it is on screen before any request returns,
  // and each block below fills in when its own answer does.
  //
  // `PersonRow | null`: `null` is a loaded answer meaning "no profile row", the
  // anonymous case. `view.hasData` is what tells it from "not loaded yet".
  const view = new CachedView<PersonRow | null>();
  const timelineView = new CachedView<PersonTimeline>();

  // `reload()` replays each view's most recent key and fetcher with force,
  // which is exactly what Refresh means here — the page's loads are driven by
  // effects, so there is no `load(force)` to call and reconstructing the key
  // by hand would be a second definition of it, free to drift.
  const refresher = pageRefresher(async () => {
    await Promise.all([view.reload(), timelineView.reload()]);
  });

  const user = $derived(view.data ?? null);
  const userLoading = $derived(view.loading && !view.hasData);
  const events = $derived(timelineView.data?.events ?? []);
  const errors = $derived(timelineView.data?.errors ?? []);
  const revalidating = $derived(view.revalidating || timelineView.revalidating);
  // The page as a whole fails only when it has NOTHING to show. One section
  // failing beside one that loaded is reported inside that section.
  const error = $derived(
    !view.hasData && !timelineView.hasData && !view.loading && !timelineView.loading
      ? (view.error ?? timelineView.error)
      : null,
  );
  /** Whichever view failed, handed to `ErrorState` for its human sentence. */
  const failedView = $derived(view.error ? view : timelineView);
  /**
   * A 404 is a fact about this id, not a transport failure: "not found" and
   * the way back, never "Couldn't load" with a Retry that can only 404 again.
   * (A distinct id with no profile row is NOT this — the summary answers 200
   * with `user: null`, and the page shows it as anonymous.)
   */
  const notFound = $derived(!!error && failedView.errorStatus === 404);

  type TimelineItem =
    | { kind: 'event'; at: number; data: AnalyticsEvent }
    | { kind: 'error'; at: number; data: ErrorEvent };

  const timeline = $derived.by<TimelineItem[]>(() => {
    const items: TimelineItem[] = [];
    for (const e of events) {
      items.push({ kind: 'event', at: new Date(e.occurred_at).getTime(), data: e });
    }
    for (const err of errors) {
      items.push({ kind: 'error', at: new Date(err.occurred_at).getTime(), data: err });
    }
    return items.sort((a, b) => b.at - a.at);
  });

  /**
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor adds to the request but which appears in none of these
   * arguments — omit it and one environment's profile would be served as another's.
   *
   * `force` bypasses the fresh-window short-circuit, for a caller that means
   * "go to the network now".
   */
  async function load(appId: string, id: string, force = false) {
    // Started together, settled separately. `LIMIT` is in the timeline's key
    // only: it is the timeline's depth and the profile row has none.
    await Promise.allSettled([
      view.load(
        viewKey('persons.profile', appId, sessionStore.scopeKey, id),
        () => getPersonSummary(appId, id),
        force,
      ),
      timelineView.load(
        viewKey('persons.profile.timeline', appId, sessionStore.scopeKey, id, LIMIT),
        () => getPersonTimeline(appId, id, LIMIT),
        force,
      ),
    ]);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const id = distinctId;
    if (aid && id) void load(aid, id);
  });

  function errorTitle(e: ErrorEvent): string {
    const type = e.exception_type ?? t('timeline.kind.error');
    const val = e.exception_value ?? e.message ?? '';
    return val ? `${type}: ${val}` : type;
  }

  // session_id is typed on AnalyticsEvent but may also ride along on error
  // payloads at runtime — read it defensively without widening the shared model.
  function sessionIdOf(data: AnalyticsEvent | ErrorEvent): string | null {
    const sid = (data as { session_id?: string | null }).session_id;
    return sid && sid.length > 0 ? sid : null;
  }

  const sessionCount = $derived.by(() => {
    const ids = new Set<string>();
    for (const e of events) {
      const sid = sessionIdOf(e);
      if (sid) ids.add(sid);
    }
    for (const e of errors) {
      const sid = sessionIdOf(e);
      if (sid) ids.add(sid);
    }
    return ids.size;
  });

  /**
   * The three tiles: the person's TOTALS from the summary — the person rollup,
   * the same numbers the Users list shows for them — never a count of the
   * timeline's rows. The timeline is capped at `LIMIT` per kind, so counting it
   * read "100" for a person with thousands; and before the timeline read the
   * archive, a person older than the deployment's hot window read "0" beside a
   * Users row saying 21.
   *
   * Never FEWER than the timeline lists, though. On an app whose person
   * rollup was never backfilled, the summary still counts the rows Postgres
   * holds, while the timeline also reads the archive — taken alone, the
   * totals would read "0" above a list of archived rows, the very
   * contradiction this replaced.
   *
   * A person with no profile row (`user: null`) has no totals, so the tiles
   * fall back to what the timeline holds. `null` until there is something to
   * show either way.
   */
  const totals = $derived.by(() => {
    const listed = timelineView.hasData
      ? { events: events.length, errors: errors.length, sessions: sessionCount }
      : null;
    if (user) {
      return {
        events: Math.max(user.events_count, listed?.events ?? 0),
        errors: Math.max(user.errors_count, listed?.errors ?? 0),
        sessions: Math.max(user.sessions_count, listed?.sessions ?? 0),
      };
    }
    if (view.hasData && listed) return listed;
    return null;
  });
  /** The server could not read some archived rows; the lists are Postgres's. */
  const archiveIncomplete = $derived(timelineView.data?.archive_incomplete === true);
  const totalsLoading = $derived(!totals && (view.loading || timelineView.loading));
  /** The timeline lists fewer rows than the totals count — say so under it. */
  const timelineCapped = $derived(
    !!user && timelineView.hasData && (user.events_count > events.length || user.errors_count > errors.length),
  );

  const hasTraits = $derived(!!user?.properties && Object.keys(user.properties).length > 0);

  /**
   * What the timeline's trailing offsets read against. Deliberately not
   * persisted: "since the first entry shown" is the right first answer for a
   * profile you have just opened, and a remembered delta mode would silently
   * change what every future profile's numbers mean.
   */
  let timeMode = $state<PersonTimeMode>('start');

  /**
   * The trailing offset label. An em dash — not a `+` with nothing after it —
   * when there is no reference point, which in `delta` mode is the last row.
   */
  function offsetLabel(i: number): string {
    const ms = personOffsetMs(timeline, i, timeMode);
    return ms === null ? '—' : `+${formatOffset(ms)}`;
  }

  /**
   * The timeline as a file. `at` goes out as an ISO instant rather than the
   * epoch milliseconds the component sorts on, and the rows keep the
   * newest-first order the card renders — the button sits in the timeline's
   * header, so what it hands over should be what is on screen.
   *
   * Worth knowing when reading an export: the profile pulls a capped window of
   * events and a capped window of errors *separately*, so on a busy person the
   * two cover different spans and the tail of this list is not a complete
   * record of that period.
   */
  function downloadPersonJson() {
    // Not before both have landed: the file would otherwise claim a person
    // with no profile, or no history, when neither had been read yet.
    if (!view.hasData || !timelineView.hasData) return;
    const exportData = {
      distinct_id: distinctId,
      user,
      timeline: timeline.map((item) => ({
        kind: item.kind,
        at: new Date(item.at).toISOString(),
        data: item.data,
      })),
    };
    const blob = new Blob([JSON.stringify(exportData, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = personJsonFilename(distinctId);
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }
</script>

  <!-- Rendered from the URL alone, before either request returns: the id is
       the one thing this page knows for certain the moment it mounts. -->
  <PageHeader
    title={distinctId}
    titleClass="mono"
    back={{ label: t('nav.users'), href: '#/users' }}
  >
    {#snippet leading()}
      <span class="avatar" aria-hidden="true">{initials(distinctId)}</span>
    {/snippet}
    {#snippet meta()}
      <CopyButton value={distinctId} size="sm" />
      {#if userLoading}
        <span class="meta-skel"><Skeleton rows={1} height="13px" /></span>
      {:else if user}
        <span>{t('explore.column.firstSeen')} <TimeValue value={user.first_seen} /></span>
        <span>{t('explore.column.lastSeen')} <TimeValue value={user.last_seen} /></span>
      {:else if view.hasData}
        <span>{t('person.anonymousNote')}</span>
      {/if}
    {/snippet}
    {#snippet freshness()}
      {#if view.hasData}<Freshness fetchedAt={view.fetchedAt} {revalidating} />{/if}
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || revalidating} />
    {/snippet}
  </PageHeader>

  {#if notFound}
    <EmptyState
      title={t('person.notFound.title')}
      description={t('person.notFound.body')}
      icon="inbox"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/users')}>{t('person.backToUsers')}</Button>
      {/snippet}
    </EmptyState>
  {:else if error}
    <ErrorState title={t('person.error.load')} error={failedView} onretry={refresher.run} />
  {:else}
    <!-- The three counts are the profile's totals (see `totals`). First and
         last seen are the profile's too, and sit in the header's meta line. -->
    <StatTiles min={140}>
      <StatTile
        label={t('explore.column.events')}
        value={totals ? formatNumber(totals.events) : '—'}
        loading={totalsLoading}
      />
      <StatTile
        label={t('explore.column.errors')}
        value={totals ? formatNumber(totals.errors) : '—'}
        tone={(totals?.errors ?? 0) > 0 ? 'error' : 'neutral'}
        loading={totalsLoading}
      />
      <StatTile
        label={t('explore.column.sessions')}
        value={totals ? formatNumber(totals.sessions) : '—'}
        loading={totalsLoading}
      />
    </StatTiles>

    <div class="detail-layout">
      <div class="col-main">
        <Card title={t('person.card.timeline')}>
          {#snippet actions()}
            {#if timeline.length > 0}
              <Button
                variant="ghost"
                size="sm"
                title={t('person.downloadTitle')}
                onclick={downloadPersonJson}
              >
                <Icon name="download" size={14} />
                {t('explore.downloadJson')}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                title={timeMode === 'delta'
                  ? t('person.timeMode.deltaTitle')
                  : t('person.timeMode.firstTitle')}
                onclick={() => (timeMode = timeMode === 'delta' ? 'start' : 'delta')}
              >
                <Icon name="clock" size={14} />
                {timeMode === 'delta' ? t('timeline.mode.sincePrevious') : t('person.timeMode.sinceFirst')}
              </Button>
            {/if}
          {/snippet}
          <ViewSection view={timelineView} rows={8}>
          {#if archiveIncomplete}
            <p class="tl-capped" role="status">{t('timeline.archiveIncomplete')}</p>
          {/if}
          {#if timeline.length === 0}
            <EmptyState size="section" title={t('person.empty.title')} description={t('person.empty.body')} />
          {:else}
            <ol class="timeline">
              {#each timeline as item, i (item.kind + i)}
                {@const sid = sessionIdOf(item.data)}
                <li class="tl-item">
                  <span class="tl-node">
                    <span class="tl-dot {item.kind}"></span>
                    {#if i < timeline.length - 1}<span class="tl-line"></span>{/if}
                  </span>
                  <div class="tl-body">
                    <div class="tl-top">
                      {#if item.kind === 'error'}
                        <LevelBadge level={item.data.level} size="sm" />
                        <button
                          class="tl-title link-title"
                          onclick={() => push(`/issues/${item.data.issue_id}`)}
                        >
                          {errorTitle(item.data)}
                        </button>
                      {:else}
                        <Badge tone="info" size="sm">{t('timeline.kind.event')}</Badge>
                        <span class="tl-title mono">{item.data.name}</span>
                      {/if}
                      {#if sid}
                        <a class="tl-session" href={`#/sessions/${encodeURIComponent(sid)}`}>
                          <Badge tone="neutral" size="sm">
                            {t('sessions.column.session')}
                            <Icon name="arrow-up-right" size={12} />
                          </Badge>
                        </a>
                      {/if}
                      <span class="tl-time"><TimeValue value={item.data.occurred_at} /></span>
                      <span
                        class="tl-offset mono"
                        title={timeMode === 'delta'
                          ? t('timeline.offset.sincePrevious')
                          : t('person.offset.sinceFirst')}
                      >{offsetLabel(i)}</span>
                    </div>
                    {#if item.kind === 'event' && item.data.properties && Object.keys(item.data.properties).length > 0}
                      <div class="tl-props">
                        {#each Object.entries(item.data.properties) as [k, v] (k)}
                          <Badge tone="neutral" size="sm">
                            <span class="prop mono" title={`${k}: ${String(v)}`}>{k}: {String(v)}</span>
                          </Badge>
                        {/each}
                      </div>
                    {/if}
                    {#if item.kind === 'error' && item.data.release}
                      <div class="tl-props">
                        <Badge tone="neutral" size="sm">
                          <span class="prop mono">{t('session.meta.release', { release: item.data.release })}</span>
                        </Badge>
                      </div>
                    {/if}
                  </div>
                </li>
              {/each}
            </ol>
            {#if timelineCapped}
              <p class="tl-capped">
                {t('person.timeline.capped', {
                  events: formatNumber(events.length),
                  errors: formatNumber(errors.length),
                })}
              </p>
            {/if}
          {/if}
          </ViewSection>
        </Card>
      </div>

      <!-- No Identity card: it repeated the distinct id the header already
           shows (with its Copy button) and, for an anonymous profile, the note
           the header's meta line already carries. -->
      <aside class="col-side">
        <Card title={t('users.column.traits')}>
          {#if userLoading}
            <Skeleton rows={3} />
          {:else if hasTraits}
            <JsonTree value={user?.properties} expandTo={2} />
          {:else}
            <EmptyState size="section" title={t('person.noTraits')} />
          {/if}
        </Card>
      </aside>
    </div>
  {/if}

<style>
  .avatar {
    width: 44px;
    height: 44px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    color: var(--text-muted);
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
    flex-shrink: 0;
  }
  .meta-skel {
    display: inline-block;
    width: 220px;
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
  .timeline {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .tl-item {
    display: flex;
    gap: 13px;
  }
  .tl-node {
    position: relative;
    display: flex;
    justify-content: center;
    width: 12px;
    flex-shrink: 0;
    padding-top: 6px;
  }
  .tl-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    z-index: 1;
    box-shadow: 0 0 0 3px var(--bg);
  }
  .tl-dot.error {
    background: var(--error);
  }
  .tl-dot.event {
    background: var(--info);
  }
  .tl-line {
    position: absolute;
    top: 14px;
    bottom: -8px;
    width: 2px;
    background: var(--border);
  }
  .tl-body {
    padding-bottom: 18px;
    min-width: 0;
    flex: 1;
  }
  .tl-top {
    display: flex;
    align-items: center;
    gap: 9px;
    flex-wrap: wrap;
  }
  .tl-title {
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    color: var(--text);
  }
  .link-title {
    background: none;
    border: none;
    padding: 0;
    text-align: start;
    cursor: pointer;
  }
  .link-title:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .tl-session {
    display: inline-flex;
    text-decoration: none;
  }
  .tl-session:hover :global(.badge) {
    color: var(--text);
  }
  .tl-time {
    font-size: var(--text-xs);
    color: var(--text-faint);
    margin-inline-start: auto;
  }
  .tl-capped {
    margin: 12px 0 0;
    font-size: var(--text-xs);
    color: var(--text-faint);
  }
  /* Tabular figures so the column of offsets stays aligned as the digits
     change; `min-width` keeps the timestamp beside it from shifting when a
     label swaps between "1.0s" and "30d 00h". */
  .tl-offset {
    font-size: var(--text-xs);
    color: var(--text-faint);
    min-width: 58px;
    text-align: end;
    font-variant-numeric: tabular-nums;
  }
  .tl-props {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 7px;
    min-width: 0;
  }
  /* A property value is arbitrary text inside a one-line badge: bounded, with
     the whole pair on the `title`. */
  .prop {
    display: inline-block;
    max-width: 48ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
