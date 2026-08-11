<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
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
  import { getDevice } from '../lib/api/devices';
  import { relativeTime, formatTimestamp, formatDuration, durationBetween } from '../lib/utils/format';
  import { timeFormatStore } from '../lib/stores/time-format.svelte';
  import {
    DEVICE_PERF_DEFAULT_SORT,
    DEVICE_SESSION_DEFAULT_SORT,
    devicePerfAccessor,
    deviceSessionAccessor,
  } from '../lib/models/device-detail-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import type { DeviceDetail, ErrorEvent, Session } from '../lib/models';

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
  // `revalidating` is deliberately not surfaced: this page has no RefreshButton
  // to spin, and the payload is replaced in place when the refresh lands.
  const view = new CachedView<DeviceDetail>();

  const detail = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  const error = $derived(view.error);

  // `scopeKey` belongs in the key: it carries the selected environment, which the
  // axios interceptor adds to the request but which appears in none of these
  // arguments. Omit it and one environment's device would be served as another's.
  //
  // `force` bypasses the fresh-window short-circuit, for a call site that means
  // "go to the network now".
  async function load(appId: string, key: string, force = false) {
    await view.load(
      viewKey('devices.detail', appId, sessionStore.scopeKey, key),
      () => getDevice(appId, key),
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const key = deviceKey;
    if (aid && key) void load(aid, key);
  });

  const device = $derived(detail?.device ?? null);
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
    sortRows(detail?.sessions ?? [], deviceSessionAccessor(sessionSort.key), sessionSort.dir),
  );
  const sortedPerf = $derived(
    sortRows(detail?.perf ?? [], devicePerfAccessor(perfSort.key), perfSort.dir),
  );

  function onSessionSort(key: string, columnDefault: SortDir) {
    sessionSort = toggleSort(sessionSort, key, columnDefault);
  }
  function onPerfSort(key: string, columnDefault: SortDir) {
    perfSort = toggleSort(perfSort, key, columnDefault);
  }

  function errorTitle(e: ErrorEvent): string {
    const type = e.exception_type ?? 'Error';
    const val = e.exception_value ?? e.message ?? '';
    return val ? `${type}: ${val}` : type;
  }
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/devices')}>
    <Icon name="arrow-left" size={14} />
    Devices
  </button>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <EmptyState title="Device not found" description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/devices')}>Back to devices</Button>
      {/snippet}
    </EmptyState>
  {:else if detail && device}
    <header class="detail-head">
      <div class="head-main">
        <h1 class="dev-title">{title}</h1>
        <div class="key-row">
          <span class="key mono">{device.device_key}</span>
          <CopyButton value={device.device_key} size="sm" label="Copy key" />
        </div>
      </div>
    </header>

    <StatTiles min={150}>
      <StatTile label="Sessions" value={detail.sessions.length.toLocaleString()} />
      <StatTile label="Events" value={device.events_count.toLocaleString()} />
      <StatTile
        label="Errors"
        value={device.errors_count.toLocaleString()}
        tone={device.errors_count > 0 ? 'error' : 'neutral'}
      />
      <StatTile
        label="First seen"
        value={timeFormatStore.mode === 'relative' ? relativeTime(device.first_seen) : formatTimestamp(device.first_seen)}
        sub={timeFormatStore.mode === 'relative' ? formatTimestamp(device.first_seen) : relativeTime(device.first_seen)}
      />
      <StatTile
        label="Last seen"
        value={timeFormatStore.mode === 'relative' ? relativeTime(device.last_seen) : formatTimestamp(device.last_seen)}
        sub={timeFormatStore.mode === 'relative' ? formatTimestamp(device.last_seen) : relativeTime(device.last_seen)}
      />
    </StatTiles>

    <div class="grid">
      <div class="col-main">
        <Card title="Recent sessions" padding="none">
          {#if sortedSessions.length === 0}
            <p class="empty-note muted">No sessions recorded for this device.</p>
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <SortableTh key="session" columnDefault="asc" sort={sessionSort} onsort={onSessionSort}>
                    Session
                  </SortableTh>
                  <SortableTh key="started" sort={sessionSort} onsort={onSessionSort}>
                    Started
                  </SortableTh>
                  <SortableTh key="duration" sort={sessionSort} onsort={onSessionSort}>
                    Duration
                  </SortableTh>
                  <SortableTh key="events" class="num" sort={sessionSort} onsort={onSessionSort}>
                    Events
                  </SortableTh>
                  <SortableTh key="errors" class="num" sort={sessionSort} onsort={onSessionSort}>
                    Errors
                  </SortableTh>
                </tr>
              {/snippet}
              {#each sortedSessions as s (s.id)}
                <tr
                  class="clickable"
                  onclick={() => push('/sessions/' + encodeURIComponent(s.session_id))}
                >
                  <td>
                    <a
                      class="lnk mono truncate"
                      href={`#/sessions/${encodeURIComponent(s.session_id)}`}
                      onclick={(e) => e.stopPropagation()}
                    >
                      {s.session_id}
                    </a>
                  </td>
                  <td><TimeValue value={s.started_at} /></td>
                  <td class="cell-muted">{formatDuration(sessionDuration(s))}</td>
                  <td class="num">{s.events_count.toLocaleString()}</td>
                  <td class="num">
                    <span class:err={s.errors_count > 0}>{s.errors_count.toLocaleString()}</span>
                  </td>
                </tr>
              {/each}
            </DataTable>
          {/if}
        </Card>

        <Card title="Performance profile" padding="none">
          {#if sortedPerf.length === 0}
            <p class="empty-note muted">No performance data yet.</p>
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <SortableTh key="name" columnDefault="asc" sort={perfSort} onsort={onPerfSort}>
                    Name
                  </SortableTh>
                  <SortableTh key="op" columnDefault="asc" sort={perfSort} onsort={onPerfSort}>
                    Op
                  </SortableTh>
                  <SortableTh key="p95" class="num" sort={perfSort} onsort={onPerfSort}>p95</SortableTh>
                  <SortableTh key="count" class="num" sort={perfSort} onsort={onPerfSort}>
                    Count
                  </SortableTh>
                </tr>
              {/snippet}
              {#each sortedPerf as p (p.op + ':' + p.name)}
                <tr>
                  <td><span class="mono truncate perf-name">{p.name}</span></td>
                  <td><Badge tone="neutral" size="sm">{p.op}</Badge></td>
                  <td class="num"><LatencyBadge ms={p.p95} size="sm" /></td>
                  <td class="num">{p.count.toLocaleString()}</td>
                </tr>
              {/each}
            </DataTable>
          {/if}
        </Card>
      </div>

      <aside class="col-side">
        <Card title="Hardware & OS">
          <dl class="kv">
            <div class="kv-row"><dt>Family</dt><dd>{device.family ?? '—'}</dd></div>
            <div class="kv-row"><dt>Model</dt><dd>{device.model ?? '—'}</dd></div>
            <div class="kv-row"><dt>OS</dt><dd>{device.os_name ?? '—'}</dd></div>
            <div class="kv-row"><dt>OS version</dt><dd class="mono">{device.os_version ?? '—'}</dd></div>
            <div class="kv-row"><dt>Arch</dt><dd class="mono">{device.arch ?? '—'}</dd></div>
            <div class="kv-row"><dt>Browser</dt><dd>{device.browser ?? '—'}</dd></div>
            <div class="kv-row">
              <dt>Last user</dt>
              <dd>
                {#if device.last_distinct_id}
                  <a
                    class="lnk mono"
                    href={`#/persons/${encodeURIComponent(device.last_distinct_id)}`}
                  >
                    {device.last_distinct_id}
                  </a>
                {:else}
                  —
                {/if}
              </dd>
            </div>
          </dl>
        </Card>

        <Card title="Crash history">
          {#if detail.errors.length === 0}
            <p class="empty-note muted">No crashes reported on this device.</p>
          {:else}
            <ul class="crashes">
              {#each detail.errors as e (e.id)}
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
        </Card>
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
  .detail-head {
    margin-bottom: 20px;
  }
  .dev-title {
    font-size: 22px;
    font-weight: 660;
    line-height: 1.3;
    word-break: break-word;
  }
  .key-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 8px;
    flex-wrap: wrap;
  }
  .key {
    font-size: 12.5px;
    color: var(--text-muted);
    word-break: break-all;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 320px;
    gap: 18px;
    align-items: start;
    margin-top: 20px;
  }
  .col-main {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .empty-note {
    font-size: 13px;
    padding: 18px;
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
    font-size: 12px;
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
  .kv {
    display: flex;
    flex-direction: column;
    margin: 0;
    gap: 11px;
  }
  .kv-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .kv-row dt {
    font-size: 12px;
    color: var(--text-faint);
    flex-shrink: 0;
  }
  .kv-row dd {
    margin: 0;
    font-size: 12.5px;
    color: var(--text);
    text-align: right;
    word-break: break-word;
    min-width: 0;
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
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .crash-title {
    font-size: 12px;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
