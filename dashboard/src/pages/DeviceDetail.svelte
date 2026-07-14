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
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getDevice } from '../lib/api/devices';
  import { errorMessage } from '../lib/api/client';
  import { relativeTime, formatDateTime, formatDuration, durationBetween } from '../lib/utils/format';
  import type { DeviceDetail, ErrorEvent, Session } from '../lib/models';

  interface Props {
    params?: { key?: string };
  }
  let { params }: Props = $props();

  const deviceKey = $derived(decodeURIComponent(params?.key ?? ''));

  let detail = $state<DeviceDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(appId: string, key: string) {
    loading = true;
    error = null;
    try {
      detail = await getDevice(appId, key);
    } catch (err) {
      error = errorMessage(err);
      detail = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
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
      <StatTile label="First seen" value={formatDateTime(device.first_seen)} />
      <StatTile label="Last seen" value={relativeTime(device.last_seen)} sub={formatDateTime(device.last_seen)} />
    </StatTiles>

    <div class="grid">
      <div class="col-main">
        <Card title="Recent sessions" padding="none">
          {#if detail.sessions.length === 0}
            <p class="empty-note muted">No sessions recorded for this device.</p>
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <th>Session</th>
                  <th>Started</th>
                  <th>Duration</th>
                  <th class="num">Events</th>
                  <th class="num">Errors</th>
                </tr>
              {/snippet}
              {#each detail.sessions as s (s.id)}
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
                  <td class="cell-muted" title={formatDateTime(s.started_at)}>
                    {relativeTime(s.started_at)}
                  </td>
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
          {#if detail.perf.length === 0}
            <p class="empty-note muted">No performance data yet.</p>
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <th>Name</th>
                  <th>Op</th>
                  <th class="num">p95</th>
                  <th class="num">Count</th>
                </tr>
              {/snippet}
              {#each detail.perf as p (p.op + ':' + p.name)}
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
                      <span class="crash-time" title={formatDateTime(e.occurred_at)}>
                        {relativeTime(e.occurred_at)}
                      </span>
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
