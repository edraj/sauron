<script lang="ts">
  import { t } from '../lib/i18n';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { formatNumber } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import { rowHref, rowNav } from '../lib/utils/row-link';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import FactList from '../lib/components/FactList.svelte';
  import Fact from '../lib/components/Fact.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import {
    getDeviceSummary,
    getDeviceSessions,
    getDeviceErrors,
    getDevicePerf,
  } from '../lib/api/devices';
  import { formatDuration, durationBetween } from '../lib/utils/format';
  import {
    DEVICE_PERF_DEFAULT_SORT,
    DEVICE_SESSION_DEFAULT_SORT,
    devicePerfAccessor,
    deviceSessionAccessor,
  } from '../lib/models/device-detail-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import type { DeviceRow, ErrorEvent, PerfSummaryRow, Session } from '../lib/models';
  import Freshness from '../lib/components/ui/Freshness.svelte';

  interface Props {
    params?: { key?: string };
  }
  let { params }: Props = $props();

  const deviceKey = $derived(decodeURIComponent(params?.key ?? ''));

  // Cached view (lib/stores/cached-view.svelte.ts): a device visited a moment ago
  // paints instantly on return and refreshes behind the rendered page instead of
  // blanking to a spinner. Re-exposed under the names the template already used,
  // so the markup is unchanged.
  //
  // `revalidating` now IS surfaced: the page grew a RefreshButton, and a
  // refresh that spun nothing while the payload swapped underneath read as the
  // click having done nothing.
  //
  // FOUR views. The device row is a keyed lookup; its sessions, its errors and
  // the p95 aggregate behind the performance table are three separate reads,
  // and as one payload the slowest of them held the header. Read apart, the
  // page says what the device IS at once and each card fills in on its own.
  const view = new CachedView<DeviceRow>();
  const sessionsView = new CachedView<Session[]>();
  const errorsView = new CachedView<ErrorEvent[]>();
  const perfView = new CachedView<PerfSummaryRow[]>();
  const sections = [view, sessionsView, errorsView, perfView];

  const refresher = pageRefresher(async () => {
    await Promise.all(sections.map((v) => v.reload()));
  });

  const loading = $derived(view.loading);
  const error = $derived(view.error);
  /**
   * A 404 on the device row is a fact about this device, not a transport
   * failure: it gets "not found" and the way back, never "Couldn't load" with a
   * Retry that can only 404 again. Every other failure is a load error.
   */
  const notFound = $derived(!loading && !!error && view.errorStatus === 404);
  const revalidating = $derived(
    view.revalidating ||
      sessionsView.revalidating ||
      errorsView.revalidating ||
      perfView.revalidating,
  );

  // `scopeKey` belongs in the key: it carries the selected environment, which the
  // axios interceptor adds to the request but which appears in none of these
  // arguments. Omit it and one environment's device would be served as another's.
  //
  // `force` bypasses the fresh-window short-circuit, for a call site that means
  // "go to the network now".
  //
  // Started together, settled separately — see the note on the four views.
  async function load(appId: string, key: string, force = false) {
    const scope = sessionStore.scopeKey;
    await Promise.allSettled([
      view.load(
        viewKey('devices.detail', appId, scope, key),
        () => getDeviceSummary(appId, key),
        force,
      ),
      sessionsView.load(
        viewKey('devices.detail.sessions', appId, scope, key),
        () => getDeviceSessions(appId, key),
        force,
      ),
      errorsView.load(
        viewKey('devices.detail.errors', appId, scope, key),
        () => getDeviceErrors(appId, key),
        force,
      ),
      perfView.load(
        viewKey('devices.detail.perf', appId, scope, key),
        () => getDevicePerf(appId, key),
        force,
      ),
    ]);
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const key = deviceKey;
    if (aid && key) void load(aid, key);
  });

  const device = $derived(view.data ?? null);
  const title = $derived.by(() => {
    if (!device) return deviceKey;
    const label = [device.family, device.model].filter(Boolean).join(' ').trim();
    return label || device.device_key;
  });

  function sessionDuration(s: Session): number {
    return durationBetween(s.started_at, s.last_event_at);
  }

  // Both panels arrive whole — the sessions list is the server's 50 most
  // recent and the perf profile its top 100 operations — so the sort runs over
  // the ENTIRE array each table renders. Neither gets a pager: what is on
  // screen is already everything the query returned, and a pager here would
  // imply a page two that does not exist.
  //
  // A bare `SortState` per table, not the `OffsetListState` the paginated
  // tables use: that type exists to make "apply a sort" and "reset to page 1"
  // one indivisible step, and with no offset there is nothing to reset. Its
  // `key`/`dir` are `readonly` (see `sort.ts`), so `sessionSort.dir = 'asc'`
  // is a type error and every transition goes through `toggleSort`.
  let sessionSort = $state<SortState>(DEVICE_SESSION_DEFAULT_SORT);
  let perfSort = $state<SortState>(DEVICE_PERF_DEFAULT_SORT);

  // `sortRows` copies before sorting. That is load-bearing, not tidy:
  // `view.data` is the VERY OBJECT the view cache holds, handed back by
  // reference (`cached-view.svelte.ts` says so, and `$state.raw` keeps that
  // identity exact), so an in-place sort would reorder the cached payload for
  // every later reader and the ordering would survive into the next visit to
  // this device. No runes machinery prevents that — only copying does.
  const sortedSessions = $derived(
    sortRows(sessionsView.data ?? [], deviceSessionAccessor(sessionSort.key), sessionSort.dir),
  );
  const sortedPerf = $derived(
    sortRows(perfView.data ?? [], devicePerfAccessor(perfSort.key), perfSort.dir),
  );

  function onSessionSort(key: string, columnDefault: SortDir) {
    sessionSort = toggleSort(sessionSort, key, columnDefault);
  }
  function onPerfSort(key: string, columnDefault: SortDir) {
    perfSort = toggleSort(perfSort, key, columnDefault);
  }

  function errorTitle(e: ErrorEvent): string {
    const type = e.exception_type ?? t('timeline.kind.error');
    const val = e.exception_value ?? e.message ?? '';
    return val ? `${type}: ${val}` : type;
  }
</script>

  <!-- The title and the key come from the URL until the row lands, so the
       heading is on screen before any request returns. -->
  <PageHeader
    {title}
    titleClass={title === deviceKey ? 'mono' : ''}
    back={{ label: t('nav.devices'), href: '#/devices' }}
  >
    {#snippet meta()}
      <span class="meta-id">
        <span class="mono">{deviceKey}</span>
        <CopyButton value={deviceKey} size="sm" />
      </span>
      {#if device}
        <span>{t('explore.column.firstSeen')} <TimeValue value={device.first_seen} /></span>
        <span>{t('explore.column.lastSeen')} <TimeValue value={device.last_seen} /></span>
      {/if}
    {/snippet}
    {#snippet freshness()}
      {#if device}<Freshness fetchedAt={view.fetchedAt} {revalidating} />{/if}
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || revalidating} />
    {/snippet}
  </PageHeader>

  <!-- The device ROW decides whether there is a page at all; every card below
       is a section of a page that exists, and waits and fails on its own. -->
  {#if notFound}
    <EmptyState title={t('device.notFound')} description={t('device.notFound.body')} icon="inbox">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/devices')}>{t('device.backToList')}</Button>
      {/snippet}
    </EmptyState>
  {:else if !loading && error}
    <ErrorState
      title={t('device.error.load')}
      error={view}
      onretry={() => sessionStore.currentAppId && load(sessionStore.currentAppId, deviceKey, true)}
    />
  {:else}
    <!-- The first tile counts the sessions list; the other two read the row.
         First and last seen sit in the header's meta line, where TimeValue can
         toggle them like every other timestamp. -->
    <StatTiles min={150}>
      <StatTile
        label={t('explore.column.sessions')}
        value={sessionsView.data ? formatNumber(sessionsView.data.length) : '—'}
        loading={sessionsView.loading && !sessionsView.hasData}
      />
      <StatTile
        label={t('explore.column.events')}
        value={formatNumber(device?.events_count ?? 0)}
        loading={!device}
      />
      <StatTile
        label={t('explore.column.errors')}
        value={formatNumber(device?.errors_count ?? 0)}
        tone={(device?.errors_count ?? 0) > 0 ? 'error' : 'neutral'}
        loading={!device}
      />
    </StatTiles>

    <div class="detail-layout">
      <div class="col-main">
        <Card title={t('device.card.sessions')}>
          <ViewSection view={sessionsView} rows={5}>
          {#if sortedSessions.length === 0}
            <EmptyState size="section" title={t('device.empty.sessionsTitle')} />
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <SortableTh key="session" columnDefault="asc" sort={sessionSort} onsort={onSessionSort}>
                    {t('sessions.column.session')}
                  </SortableTh>
                  <SortableTh key="started" sort={sessionSort} onsort={onSessionSort}>
                    {t('explore.column.started')}
                  </SortableTh>
                  <SortableTh key="duration" class="num" sort={sessionSort} onsort={onSessionSort}>
                    {t('explore.column.duration')}
                  </SortableTh>
                  <SortableTh key="events" class="num" sort={sessionSort} onsort={onSessionSort}>
                    {t('explore.column.events')}
                  </SortableTh>
                  <SortableTh key="errors" class="num" sort={sessionSort} onsort={onSessionSort}>
                    {t('explore.column.errors')}
                  </SortableTh>
                </tr>
              {/snippet}
              {#each sortedSessions as s (s.id)}
                {@const path = '/sessions/' + encodeURIComponent(s.session_id)}
                <tr
                  class="clickable"
                  onclick={(e) => rowNav(e, path)}
                  onauxclick={(e) => rowNav(e, path)}
                >
                  <td>
                    <a class="lnk mono truncate" href={rowHref(path)}>
                      {s.session_id}
                    </a>
                  </td>
                  <td><TimeValue value={s.started_at} /></td>
                  <td class="num">{formatDuration(sessionDuration(s))}</td>
                  <td class="num">{formatNumber(s.events_count)}</td>
                  <td class="num">
                    <span class:err={s.errors_count > 0}>{formatNumber(s.errors_count)}</span>
                  </td>
                </tr>
              {/each}
            </DataTable>
          {/if}
          </ViewSection>
        </Card>

        <Card title={t('device.card.performance')}>
          <ViewSection view={perfView} rows={5}>
          {#if sortedPerf.length === 0}
            <EmptyState size="section" title={t('device.empty.performanceTitle')} />
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <SortableTh key="name" columnDefault="asc" sort={perfSort} onsort={onPerfSort}>
                    {t('common.name')}
                  </SortableTh>
                  <SortableTh key="op" columnDefault="asc" sort={perfSort} onsort={onPerfSort}>
                    {t('device.column.op')}
                  </SortableTh>
                  <SortableTh key="p95" class="num" sort={perfSort} onsort={onPerfSort}>p95</SortableTh>
                  <SortableTh key="count" class="num" sort={perfSort} onsort={onPerfSort}>
                    {t('explore.column.count')}
                  </SortableTh>
                </tr>
              {/snippet}
              {#each sortedPerf as p (p.op + ':' + p.name)}
                <tr>
                  <td><span class="mono truncate perf-name">{p.name}</span></td>
                  <td><Badge tone="neutral" size="sm">{p.op}</Badge></td>
                  <td class="num"><LatencyBadge ms={p.p95} size="sm" /></td>
                  <td class="num">{formatNumber(p.count)}</td>
                </tr>
              {/each}
            </DataTable>
          {/if}
          </ViewSection>
        </Card>
      </div>

      <aside class="col-side">
        <Card title={t('device.card.hardware')}>
          {#if !device}
            <Skeleton rows={7} />
          {:else}
            <FactList>
              <Fact label={t('device.field.family')}>{device.family ?? '—'}</Fact>
              <Fact label={t('device.field.model')}>{device.model ?? '—'}</Fact>
              <Fact label={t('device.field.os')}>{device.os_name ?? '—'}</Fact>
              <Fact label={t('device.field.osVersion')} mono>{device.os_version ?? '—'}</Fact>
              <Fact label={t('device.field.arch')} mono>{device.arch ?? '—'}</Fact>
              <Fact label={t('device.field.browser')}>{device.browser ?? '—'}</Fact>
              <Fact label={t('device.field.lastUser')} mono>
                {#if device.last_distinct_id}
                  <a class="lnk" href={`#/persons/${encodeURIComponent(device.last_distinct_id)}`}>
                    {device.last_distinct_id}
                  </a>
                {:else}
                  —
                {/if}
              </Fact>
            </FactList>
          {/if}
        </Card>

        <Card title={t('device.card.crashes')}>
          <ViewSection view={errorsView} rows={4}>
          {#if (errorsView.data ?? []).length === 0}
            <EmptyState size="section" title={t('device.empty.crashesTitle')} />
          {:else}
            <ul class="crashes">
              {#each errorsView.data ?? [] as e (e.id)}
                <li>
                  <a class="crash" href={`#/issues/${e.issue_id}`}>
                    <div class="crash-top">
                      <LevelBadge level={e.level} size="sm" />
                      <span class="crash-time"><TimeValue value={e.occurred_at} asText /></span>
                    </div>
                    <span class="crash-title mono">{errorTitle(e)}</span>
                  </a>
                </li>
              {/each}
            </ul>
          {/if}
          </ViewSection>
        </Card>
      </aside>
    </div>
  {/if}

<style>
  .meta-id {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: var(--text-2xs);
    overflow-wrap: anywhere;
  }
  /* The rail layout itself is the shared `.detail-layout` (app.css). */
  .detail-layout {
    margin-top: var(--page-gap);
  }
  .col-main {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }
  .lnk {
    color: var(--text-muted);
  }
  .lnk:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .lnk.truncate {
    display: inline-block;
    max-width: 260px;
  }
  .perf-name {
    display: inline-block;
    max-width: 220px;
    font-size: var(--text-xs);
  }
  .err {
    color: var(--error);
    font-weight: var(--weight-strong);
  }
  .crashes {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .crash {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 9px 10px;
    border-radius: var(--radius);
    border: 1px solid transparent;
    transition: background 0.12s ease, border-color 0.12s ease;
  }
  .crash:hover {
    background: var(--surface-2);
    border-color: var(--border);
  }
  .crash-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .crash-time {
    font-size: var(--text-xs);
    color: var(--text-faint);
  }
  .crash-title {
    font-size: var(--text-xs);
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
