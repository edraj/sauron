<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
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
  import { relativeTime, formatTimestamp, initials } from '../lib/utils/format';
  import { timeFormatStore } from '../lib/stores/time-format.svelte';
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
  const timelineLoading = $derived(timelineView.loading && !timelineView.hasData);
  const revalidating = $derived(view.revalidating || timelineView.revalidating);
  // The page as a whole fails only when it has NOTHING to show. One section
  // failing beside one that loaded is reported inside that section.
  const error = $derived(
    !view.hasData && !timelineView.hasData && !view.loading && !timelineView.loading
      ? (view.error ?? timelineView.error)
      : null,
  );

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
    const type = e.exception_type ?? 'Error';
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

  <button class="back" onclick={() => push('/events')}><Icon name="arrow-left" size={14} /> {t('person.backToEvents')}</button>

  {#if error}
    <EmptyState title={t('person.error.load')} description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/events')}>{t('common.back')}</Button>
      {/snippet}
    </EmptyState>
  {:else}
    <!-- Rendered from the URL alone, before either request returns: the id is
         the one thing this page knows for certain the moment it mounts. -->
    <header class="identity">
      <div class="id-actions">
        <Freshness fetchedAt={view.fetchedAt} {revalidating} />
        <RefreshButton onclick={refresher.run} loading={refresher.busy || revalidating} />
      </div>
      <span class="avatar">{initials(distinctId)}</span>
      <div class="id-meta">
        <h1 class="id-title mono">{distinctId}</h1>
        <div class="id-sub">
          {#if userLoading}
            <Skeleton rows={1} height="13px" />
          {:else if user}
            <span class="muted">
              {t('explore.column.firstSeen')} <TimeValue value={user.first_seen} /> {t('prose.person.lastSeen')}
              <TimeValue value={user.last_seen} />
            </span>
          {:else}
            <span class="muted">{t('person.anonymousNote')}</span>
          {/if}
        </div>
      </div>
    </header>

    <!-- Two requests feed this row, so its tiles fill in two at a time: the
         three counts come off the timeline, the two dates off the profile. -->
    <div class="tiles">
      <StatTiles min={140}>
        <StatTile
          label={t('explore.column.events')}
          value={timelineView.hasData ? formatNumber(events.length) : '—'}
          loading={timelineLoading}
        />
        <StatTile
          label={t('explore.column.errors')}
          value={timelineView.hasData ? formatNumber(errors.length) : '—'}
          tone={errors.length > 0 ? 'error' : 'neutral'}
          loading={timelineLoading}
        />
        <StatTile
          label={t('explore.column.sessions')}
          value={sessionCount > 0 ? formatNumber(sessionCount) : '—'}
          loading={timelineLoading}
        />
        <StatTile
          label={t('explore.column.firstSeen')}
          loading={userLoading}
          value={user
            ? timeFormatStore.mode === 'relative'
              ? relativeTime(user.first_seen)
              : formatTimestamp(user.first_seen)
            : '—'}
          sub={user
            ? timeFormatStore.mode === 'relative'
              ? formatTimestamp(user.first_seen)
              : relativeTime(user.first_seen)
            : undefined}
        />
        <StatTile
          label={t('explore.column.lastSeen')}
          loading={userLoading}
          value={user
            ? timeFormatStore.mode === 'relative'
              ? relativeTime(user.last_seen)
              : formatTimestamp(user.last_seen)
            : '—'}
          sub={user
            ? timeFormatStore.mode === 'relative'
              ? formatTimestamp(user.last_seen)
              : relativeTime(user.last_seen)
            : undefined}
        />
      </StatTiles>
    </div>

    <div class="grid">
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
                  ? 'Showing time since the previous entry — click to measure from the first entry shown'
                  : 'Showing time since the first entry shown — click to measure from the previous entry'}
                onclick={() => (timeMode = timeMode === 'delta' ? 'start' : 'delta')}
              >
                <Icon name="clock" size={14} />
                {timeMode === 'delta' ? 'Since previous' : 'Since start'}
              </Button>
            {/if}
          {/snippet}
          <ViewSection view={timelineView} rows={8}>
          {#if timeline.length === 0}
            <EmptyState title={t('person.empty.title')} description={t('person.empty.body')} icon="inbox" />
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
                        <Badge tone="info" size="sm">event</Badge>
                        <span class="tl-title mono">{item.data.name}</span>
                      {/if}
                      {#if sid}
                        <a class="tl-session mono" href={`#/sessions/${encodeURIComponent(sid)}`}>
                          session <Icon name="arrow-up-right" size={14} />
                        </a>
                      {/if}
                      <span class="tl-time"><TimeValue value={item.data.occurred_at} /></span>
                      <span
                        class="tl-offset mono"
                        title={timeMode === 'delta'
                          ? 'Since the previous entry'
                          : 'Since the first entry shown'}
                      >{offsetLabel(i)}</span>
                    </div>
                    {#if item.kind === 'event' && item.data.properties && Object.keys(item.data.properties).length > 0}
                      <div class="tl-props">
                        {#each Object.entries(item.data.properties) as [k, v] (k)}
                          <span class="prop mono">{k}: {String(v)}</span>
                        {/each}
                      </div>
                    {/if}
                    {#if item.kind === 'error'}
                      <div class="tl-props">
                        {#if item.data.release}<span class="prop mono">release: {item.data.release}</span>{/if}
                      </div>
                    {/if}
                  </div>
                </li>
              {/each}
            </ol>
          {/if}
          </ViewSection>
        </Card>
      </div>

      <aside class="col-side">
        <Card title={t('users.column.traits')}>
          {#if userLoading}
            <Skeleton rows={3} />
          {:else if hasTraits}
            <JsonTree value={user?.properties} expandTo={2} />
          {:else}
            <p class="muted empty-traits">{t('person.noTraits')}</p>
          {/if}
        </Card>
        <Card title={t('person.card.identity')}>
          <div class="summary">
            <div class="sm-row">
              <span class="muted">{t('person.distinctId')}</span>
              <span class="sm-val mono small">{distinctId}</span>
            </div>
            {#if view.hasData && !user}
              <div class="sm-row">
                <span class="muted">{t('person.title')}</span>
                <span class="sm-val small">{t('person.anonymous')}</span>
              </div>
            {/if}
          </div>
        </Card>
      </aside>
    </div>
  {/if}

<style>
  .id-actions {
    order: 99;
    margin-inline-start: auto;
    align-self: center;
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
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .identity {
    display: flex;
    align-items: center;
    gap: 16px;
    margin-bottom: 22px;
  }
  .avatar {
    width: 54px;
    height: 54px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 18px;
    font-weight: 680;
    flex-shrink: 0;
  }
  .id-title {
    font-size: 21px;
    font-weight: 660;
    word-break: break-all;
  }
  .id-sub {
    margin-top: 4px;
    font-size: 13px;
  }
  .tiles {
    margin-bottom: 18px;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 300px;
    gap: 18px;
    align-items: start;
  }
  .col-main {
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: 18px;
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
    box-shadow: 0 0 0 3px var(--surface);
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
    font-size: 13.5px;
    font-weight: 560;
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
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--text-muted);
    text-decoration: none;
    padding: 1px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--surface-2);
    white-space: nowrap;
  }
  .tl-session:hover {
    color: var(--primary);
    border-color: var(--primary-border);
  }
  .tl-time {
    font-size: 11.5px;
    color: var(--text-faint);
    margin-inline-start: auto;
  }
  /* Tabular figures so the column of offsets stays aligned as the digits
     change; `min-width` keeps the timestamp beside it from shifting when a
     label swaps between "1.0s" and "30d 00h". */
  .tl-offset {
    font-size: 11px;
    color: var(--text-faint);
    min-width: 58px;
    text-align: end;
    font-variant-numeric: tabular-nums;
  }
  .empty-traits {
    font-size: 13px;
    padding: 2px 0;
  }
  .tl-props {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 7px;
  }
  .prop {
    font-size: 11px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 2px 8px;
    border-radius: var(--radius-pill);
  }
  .summary {
    display: flex;
    flex-direction: column;
    gap: 11px;
  }
  .sm-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
  }
  .sm-val {
    font-weight: 620;
    font-variant-numeric: tabular-nums;
  }
  .sm-val.small {
    font-size: 12px;
    font-weight: 500;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
