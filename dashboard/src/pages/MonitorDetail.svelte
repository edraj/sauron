<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import { getMonitor, getMonitorChecks, updateMonitor, deleteMonitor } from '../lib/api/monitors';
  import { MONITOR_INTERVALS, formatInterval } from '../lib/constants/monitorIntervals';
  import type { MonitorDetail, MonitorCheck } from '../lib/models';
  import { sessionStore } from '../lib/stores/session.svelte';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import { formatDateTime, formatDuration, durationBetween } from '../lib/utils/format';

  let { params }: { params: { id: string } } = $props();

  let detail = $state<MonitorDetail | null>(null);
  let checks = $state<MonitorCheck[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let confirmOpen = $state(false);
  let deleting = $state(false);
  let pausing = $state(false);
  let savingInterval = $state(false);

  const canWrite = $derived(
    sessionStore.can('monitor:write', { project: detail?.monitor.project_id }),
  );

  // Sort by timestamp ourselves rather than trusting the API's order: the newest-
  // first log and the (chronological) availability strip stay correct even if the
  // endpoint's ORDER BY ever changes.
  const checksAsc = $derived(
    checks.slice().sort((a, b) => new Date(a.checked_at).getTime() - new Date(b.checked_at).getTime()),
  );
  const recentChecks = $derived(checksAsc.slice().reverse().slice(0, 100));
  const barChecks = $derived(checksAsc.slice(-60));

  async function load() {
    loading = true; error = null;
    try {
      detail = await getMonitor(params.id);
      checks = await getMonitorChecks(params.id, 24);
    } catch (e) { error = (e as Error).message; }
    finally { loading = false; }
  }

  async function togglePause() {
    if (!detail) return;
    pausing = true; error = null;
    try {
      await updateMonitor(params.id, { enabled: detail.monitor.status === 'paused' });
      await load();
    } catch (e) { error = (e as Error).message; }
    finally { pausing = false; }
  }

  async function changeInterval(e: Event) {
    const seconds = Number((e.currentTarget as HTMLSelectElement).value);
    if (!detail || seconds === detail.monitor.interval_seconds) return;
    savingInterval = true; error = null;
    try {
      await updateMonitor(params.id, { interval_seconds: seconds });
      await load();
    } catch (err) {
      error = (err as Error).message;
    } finally {
      savingInterval = false;
    }
  }

  async function remove() {
    deleting = true; error = null;
    try {
      await deleteMonitor(params.id);
      push('/monitors');
    } catch (e) {
      error = (e as Error).message;
      deleting = false;
      confirmOpen = false;
    }
  }

  const fmtPct = (v: number | null | undefined) => (v == null ? '—' : `${v.toFixed(2)}%`);
  function pctTone(v: number | null): 'neutral' | 'success' | 'warning' | 'error' {
    if (v == null) return 'neutral';
    if (v >= 99) return 'success';
    if (v >= 95) return 'warning';
    return 'error';
  }

  $effect(() => {
    if (params.id) void load();
  });
</script>

<AppShell>
  <button class="back" onclick={() => push('/monitors')}>
    <Icon name="arrow-left" size={14} />
    Uptime
  </button>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error && !detail}
    <EmptyState title="Monitor not found" description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/monitors')}>Back to Uptime</Button>
      {/snippet}
    </EmptyState>
  {:else if detail}
    {@const incidents = detail.incidents}
    <header class="detail-head">
      <div class="head-main">
        <h1 class="mon-title">{detail.monitor.name} <StatusPill status={detail.monitor.status} /></h1>
        <div class="key-row">
          <span class="kindtag">{detail.monitor.kind}</span>
          <span class="key mono">{detail.monitor.target}</span>
          <CopyButton value={detail.monitor.target} size="sm" />
        </div>
      </div>
      {#if canWrite}
        <div class="actions">
          <Button variant="secondary" loading={pausing} onclick={togglePause}>
            {detail.monitor.status === 'paused' ? 'Resume' : 'Pause'}
          </Button>
          <Button variant="danger" onclick={() => (confirmOpen = true)}>Delete</Button>
        </div>
      {/if}
    </header>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    <StatTiles min={150}>
      <StatTile label="Uptime 24h" value={fmtPct(detail.uptime.h24)} tone={pctTone(detail.uptime.h24)} />
      <StatTile label="Uptime 7d" value={fmtPct(detail.uptime.d7)} tone={pctTone(detail.uptime.d7)} />
      <StatTile label="Uptime 30d" value={fmtPct(detail.uptime.d30)} tone={pctTone(detail.uptime.d30)} />
      {#if canWrite}
        <div class="interval-tile">
          <span class="it-label">Interval</span>
          <div class="control select" class:busy={savingInterval}>
            <select
              aria-label="Check interval"
              value={detail.monitor.interval_seconds}
              disabled={savingInterval}
              onchange={changeInterval}
            >
              {#each MONITOR_INTERVALS as opt (opt.seconds)}
                <option value={opt.seconds}>{opt.label}</option>
              {/each}
            </select>
            <span class="affix">
              {#if savingInterval}
                <Spinner size={14} />
              {:else}
                <Icon name="chevron-down" size={15} />
              {/if}
            </span>
          </div>
        </div>
      {:else}
        <StatTile label="Interval" value={formatInterval(detail.monitor.interval_seconds)} />
      {/if}
    </StatTiles>

    <div class="section">
      <Card title="Recent checks" padding="none">
        {#if checks.length === 0}
          <EmptyState
            title="No checks yet"
            description="This monitor hasn't run a check yet. Results appear here once the prober reports in."
            icon="clock"
          />
        {:else}
          <div class="bar-wrap">
            <div class="uptime-bar" aria-hidden="true">
              {#each barChecks as c (c.checked_at)}
                <span
                  class="bar"
                  class:down={!c.up}
                  title={`${formatDateTime(c.checked_at)} · ${c.up ? 'up' : 'down'}${c.response_time_ms != null ? ' · ' + c.response_time_ms + ' ms' : ''}`}
                ></span>
              {/each}
            </div>
            <div class="bar-legend">
              <span>Oldest</span>
              <span>{barChecks.length} checks</span>
              <span>Newest</span>
            </div>
          </div>

          <DataTable>
            {#snippet head()}
              <tr>
                <th>Time</th>
                <th>Result</th>
                <th class="num">Code</th>
                <th class="num">Latency</th>
                <th>Error</th>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each recentChecks as c (c.checked_at)}
                <tr>
                  <td>{formatDateTime(c.checked_at)}</td>
                  <td>
                    <span class="result" class:up={c.up} class:down={!c.up}>
                      <span class="dot"></span>{c.up ? 'Up' : 'Down'}
                    </span>
                  </td>
                  <td class="num">
                    {#if c.status_code == null}<span class="faint">—</span>{:else}{c.status_code}{/if}
                  </td>
                  <td class="num">
                    {#if c.response_time_ms == null}<span class="faint">—</span>{:else}<LatencyBadge ms={c.response_time_ms} dot={false} size="sm" />{/if}
                  </td>
                  <td>
                    {#if c.error}<span class="cell-mono cell-muted errtext" title={c.error}>{c.error}</span>{:else}<span class="faint">—</span>{/if}
                  </td>
                </tr>
              {/each}
            {/snippet}
          </DataTable>
        {/if}
      </Card>
    </div>

    <div class="section">
      <Card title="Incidents" padding="none">
        {#if incidents.length === 0}
          <EmptyState
            title="No incidents"
            description="No downtime has been recorded for this monitor."
            icon="circle-check"
          />
        {:else}
          <DataTable>
            {#snippet head()}
              <tr>
                <th>Started</th>
                <th>Resolved</th>
                <th class="num">Duration</th>
                <th>Cause</th>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each incidents as i (i.id)}
                <tr>
                  <td>{formatDateTime(i.started_at)}</td>
                  <td>
                    {#if i.resolved_at}{formatDateTime(i.resolved_at)}{:else}<span class="ongoing">Ongoing</span>{/if}
                  </td>
                  <td class="num">
                    {#if i.resolved_at}{formatDuration(durationBetween(i.started_at, i.resolved_at))}{:else}<span class="faint">—</span>{/if}
                  </td>
                  <td>
                    <span class="cause">{i.cause}</span>
                    {#if i.last_error}<span class="cell-mono cell-muted errtext" title={i.last_error}>{i.last_error}</span>{/if}
                  </td>
                </tr>
              {/each}
            {/snippet}
          </DataTable>
        {/if}
      </Card>
    </div>
  {/if}
</AppShell>

<ConfirmDialog
  bind:open={confirmOpen}
  title="Delete monitor"
  message={detail ? `Delete “${detail.monitor.name}”? Its check history and incidents will be removed. This can't be undone.` : ''}
  confirmLabel="Delete monitor"
  danger
  loading={deleting}
  onconfirm={remove}
  oncancel={() => (confirmOpen = false)}
/>

<style>
  /* Editable interval tile — matches StatTile's frame with an inline select. */
  .interval-tile {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 16px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    min-width: 0;
  }
  .it-label {
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text-muted);
    text-transform: uppercase;
  }
  .interval-tile .control {
    position: relative;
    display: flex;
    align-items: center;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    transition: border-color 0.14s ease, box-shadow 0.14s ease;
  }
  .interval-tile .control:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  .interval-tile .control.busy {
    opacity: 0.7;
  }
  .interval-tile select {
    flex: 1;
    width: 100%;
    min-width: 0;
    appearance: none;
    padding: 9px 34px 9px 12px;
    font-size: 15px;
    font-weight: 560;
    background: transparent;
    border: none;
    color: var(--text);
    outline: none;
    cursor: pointer;
  }
  .interval-tile select:disabled {
    cursor: progress;
  }
  .interval-tile .affix {
    position: absolute;
    right: 11px;
    display: inline-flex;
    align-items: center;
    color: var(--text-faint);
    pointer-events: none;
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

  /* --- header --------------------------------------------------------------- */
  .detail-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 20px;
  }
  .mon-title {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
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
  .kindtag {
    font-size: 10px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 6px;
  }
  .key {
    font-size: 12.5px;
    color: var(--text-muted);
    word-break: break-all;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }

  /* --- error banner --------------------------------------------------------- */
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    margin-bottom: 18px;
    font-size: 13px;
    color: var(--error);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 38%, transparent);
    border-radius: var(--radius);
  }

  .section {
    margin-top: 18px;
  }

  /* --- availability strip (signature) --------------------------------------- */
  .bar-wrap {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 16px 18px;
    border-bottom: 1px solid var(--border);
  }
  .uptime-bar {
    display: flex;
    gap: 3px;
    align-items: stretch;
    height: 34px;
  }
  .uptime-bar .bar {
    flex: 1 1 0;
    min-width: 2px;
    border-radius: 3px;
    background: var(--success);
    opacity: 0.8;
    transition: opacity 0.1s ease;
  }
  .uptime-bar .bar.down {
    background: var(--error);
  }
  .uptime-bar .bar:hover {
    opacity: 1;
  }
  .bar-legend {
    display: flex;
    justify-content: space-between;
    font-size: 11px;
    color: var(--text-faint);
  }

  /* --- table cells ---------------------------------------------------------- */
  .result {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 560;
    font-size: 12.5px;
  }
  .result.up { color: var(--success); }
  .result.down { color: var(--error); }
  .result .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    flex-shrink: 0;
  }
  .errtext {
    display: inline-block;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
  }
  .cause {
    font-weight: 550;
    margin-right: 8px;
  }
  .ongoing {
    display: inline-flex;
    align-items: center;
    padding: 2px 9px;
    border-radius: var(--radius-pill);
    font-size: 11.5px;
    font-weight: 600;
    color: var(--warning);
    background: var(--warning-soft);
  }
</style>
