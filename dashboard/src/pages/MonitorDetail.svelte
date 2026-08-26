<script lang="ts">
  import { t } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import { getMonitor, getMonitorChecks, updateMonitor, deleteMonitor } from '../lib/api/monitors';
  import { viewCache } from '../lib/stores/view-cache';
  import { MONITOR_INTERVALS, formatInterval } from '../lib/constants/monitorIntervals';
  import type { MonitorDetail, MonitorCheck } from '../lib/models';
  import { lockedBy } from '../lib/models/page-access';
  import { lockTip } from '../lib/actions/lock-tip';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import { formatDateTime, formatDuration, durationBetween } from '../lib/utils/format';
  import {
    MONITOR_CHECK_DEFAULT_SORT,
    MONITOR_INCIDENT_DEFAULT_SORT,
    monitorCheckAccessor,
    monitorIncidentAccessor,
  } from '../lib/models/monitor-detail-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';

  let { params }: { params: { id: string } } = $props();

  let detail = $state<MonitorDetail | null>(null);
  let checks = $state<MonitorCheck[]>([]);
  // The checks half loads independently of `detail` — see load(). Its own
  // flag and error keep a slow or failed 24 h check read inside the Recent
  // checks card instead of holding up (or blanking) the whole page.
  let checksLoading = $state(false);
  let checksError = $state<string | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let confirmOpen = $state(false);
  let deleting = $state(false);
  let pausing = $state(false);
  let savingInterval = $state(false);
  let intervalConfirmOpen = $state(false);
  // What the user picked but has not confirmed yet. Kept apart from
  // `selectedInterval` (what the control displays) so a cancel can put the
  // control back without a request ever having gone out.
  let pendingInterval = $state<number | null>(null);
  // Drives the select. It has to be state rather than the monitor's stored
  // value read straight off `detail`: picking an option mutates the element
  // directly, and on cancel `detail.monitor.interval_seconds` is unchanged --
  // so an expression bound to it re-renders nothing and the control is left
  // displaying an interval the monitor is not running at.
  let selectedInterval = $state(60);

  // monitors.rs:191,223 authorize at the project.
  const writeLock = $derived(
    lockedBy('monitor:write', { project: detail?.monitor.project_id, level: 'project' }),
  );

  // Coalesced rather than read straight off the payload: during a rolling
  // upgrade the SPA can be newer than the API it is talking to, and an API
  // that predates the credential redaction omits the field entirely — which
  // would take the whole page down on `.length`, not just hide a chip.
  const probeHeaders = $derived(detail?.monitor.probe_header_names ?? []);

  // alert_rules.monitor_id is ON DELETE CASCADE, so deleting the monitor
  // silently deletes any alert rule pinned to it too. The count comes from
  // the detail payload so the operator sees it before confirming, not after
  // (the delete response also discloses it, but by then it's too late).
  const deleteMessage = $derived.by(() => {
    if (!detail) return '';
    const n = detail.pinned_alert_rules;
    const alertClause =
      n > 0
        ? ` This will also delete ${n} alert ${n === 1 ? 'rule' : 'rules'} pinned to it.`
        : '';
    return `Delete “${detail.monitor.name}”? Its check history and incidents will be removed.${alertClause} This can't be undone.`;
  });

  const intervalChangeMessage = $derived.by(() => {
    if (!detail || pendingInterval === null) return '';
    const from = formatInterval(detail.monitor.interval_seconds);
    const to = formatInterval(pendingInterval);
    return `Change the check interval for “${detail.monitor.name}” from ${from} to ${to}? The prober applies the new interval on its next cycle.`;
  });

  // Sort by timestamp ourselves rather than trusting the API's order: the newest-
  // first log and the (chronological) availability strip stay correct even if the
  // endpoint's ORDER BY ever changes.
  const checksAsc = $derived(
    checks.slice().sort((a, b) => new Date(a.checked_at).getTime() - new Date(b.checked_at).getTime()),
  );
  // WHICH hundred rows the log shows — the most recent ones. Kept as its own
  // step, and kept chronological, because it is the table's definition rather
  // than its order: `barChecks` below reads the same ascending array
  // left-to-right, and the log is "the last 100 checks" however the user then
  // chooses to order them. Replacing this with the sort would let a click on
  // Latency change which checks are on the page, not just their order.
  const recentChecks = $derived(checksAsc.slice().reverse().slice(0, 100));
  const barChecks = $derived(checksAsc.slice(-60));

  // Both tables arrive whole — 24h of checks, capped at the 100 most recent
  // for the log, and every incident this monitor has — so each sort runs over
  // the ENTIRE array its table renders. Neither gets a pager: what is on
  // screen is already all of it, and a pager would imply a page two that does
  // not exist.
  //
  // A bare `SortState` per table, not the `OffsetListState` the paginated
  // tables use: that type exists to make "apply a sort" and "reset to page 1"
  // one indivisible step, and with no offset there is nothing to reset. Its
  // `key`/`dir` are `readonly` (see `sort.ts`), so `checkSort.dir = 'asc'` is
  // a type error and every transition goes through `toggleSort`.
  //
  // The check seed reproduces the reverse-chronological order the log has
  // today, so the page opens looking exactly as it did.
  let checkSort = $state<SortState>(MONITOR_CHECK_DEFAULT_SORT);
  let incidentSort = $state<SortState>(MONITOR_INCIDENT_DEFAULT_SORT);

  // `sortRows` copies before sorting, so neither `checks` nor the payload's
  // `incidents` array is reordered in place. `recentChecks` is already a copy,
  // but `detail.incidents` is the response object's own array and this page
  // hands that same object to nothing else only by accident — copying is what
  // makes that safe rather than lucky.
  const sortedChecks = $derived(
    sortRows(recentChecks, monitorCheckAccessor(checkSort.key), checkSort.dir),
  );
  const sortedIncidents = $derived(
    sortRows(detail?.incidents ?? [], monitorIncidentAccessor(incidentSort.key), incidentSort.dir),
  );

  function onCheckSort(key: string, columnDefault: SortDir) {
    checkSort = toggleSort(checkSort, key, columnDefault);
  }
  function onIncidentSort(key: string, columnDefault: SortDir) {
    incidentSort = toggleSort(incidentSort, key, columnDefault);
  }

  async function load() {
    loading = true;
    error = null;
    checksError = null;
    checksLoading = true;
    // Both issued together as before (neither feeds the other), but no longer
    // JOINED: the header, config and actions need only `detail`, so they
    // render the moment it lands instead of waiting on 24 h of check rows —
    // and a failed checks read degrades to a message inside its own card
    // rather than blanking a page whose monitor half arrived fine.
    const checksDone = getMonitorChecks(params.id, 24)
      .then(
        (c) => {
          checks = c;
        },
        (e) => {
          checksError = (e as Error).message;
        },
      )
      .finally(() => {
        checksLoading = false;
      });
    try {
      detail = await getMonitor(params.id);
      // Reseeded here, not at mount: the router reuses this component across
      // `#/monitors/A` -> `#/monitors/B`, so a mount-time seed would leave the
      // control showing the interval of the monitor we navigated away from.
      selectedInterval = detail.monitor.interval_seconds;
      pendingInterval = null;
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
    // Callers (the pause/interval/delete refreshes) await the WHOLE load, so
    // their "refresh finished" contract still covers both halves.
    await checksDone;
  }

  async function togglePause() {
    if (!detail) return;
    pausing = true; error = null;
    try {
      await updateMonitor(params.id, { enabled: detail.monitor.status === 'paused' });
      // Monitors.svelte serves its list from the view cache, so the status column
      // there would keep showing the pre-toggle value — with no request in flight
      // to correct it — for the whole fresh window. Dropping the key is what makes
      // "Back to Uptime" refetch.
      viewCache.invalidate('monitors.list');
      await load();
    } catch (e) { error = (e as Error).message; }
    finally { pausing = false; }
  }

  // Picking an option no longer saves -- it only asks. Changing the interval
  // re-times every future check, so it goes through the same confirm step the
  // other two state-changing controls on this page use.
  function requestIntervalChange(e: Event) {
    const seconds = Number((e.currentTarget as HTMLSelectElement).value);
    if (!detail || seconds === detail.monitor.interval_seconds) return;
    pendingInterval = seconds;
    intervalConfirmOpen = true;
  }

  // Also the Escape/backdrop path: ConfirmDialog forwards Modal's `onclose`
  // here, so every way out of the dialog reverts the control.
  function cancelIntervalChange() {
    intervalConfirmOpen = false;
    pendingInterval = null;
    if (detail) selectedInterval = detail.monitor.interval_seconds;
  }

  async function confirmIntervalChange() {
    if (!detail || pendingInterval === null) return;
    const seconds = pendingInterval;
    savingInterval = true; error = null;
    try {
      await updateMonitor(params.id, { interval_seconds: seconds });
      viewCache.invalidate('monitors.list');
      intervalConfirmOpen = false;
      // Clears `pendingInterval` and reseeds the control from the saved value.
      await load();
    } catch (err) {
      error = (err as Error).message;
      // The change did not take, so the control must not keep showing it.
      cancelIntervalChange();
    } finally {
      savingInterval = false;
    }
  }

  async function remove() {
    deleting = true; error = null;
    try {
      await deleteMonitor(params.id);
      // Must happen before the navigation: Monitors.svelte's effect runs on mount
      // and would otherwise hit a fresh cache entry that still contains the row we
      // just deleted, short-circuiting the network entirely.
      viewCache.invalidate('monitors.list');
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

  <button class="back" onclick={() => push('/monitors')}>
    <Icon name="arrow-left" size={14} />
    {t('monitors.column.uptime')}
  </button>

  {#if loading}
    <Skeleton rows={6} />
  {:else if error && !detail}
    <EmptyState title={t('monitor.notFound')} description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/monitors')}>{t('monitor.backToList')}</Button>
      {/snippet}
    </EmptyState>
  {:else if detail}
    <header class="detail-head">
      <div class="head-main">
        <h1 class="mon-title">{detail.monitor.name} <StatusPill status={detail.monitor.status} /></h1>
        <div class="key-row">
          <span class="kindtag">{detail.monitor.kind}</span>
          <span class="key mono">{detail.monitor.target}</span>
          <CopyButton value={detail.monitor.target} size="sm" />
          <!--
            Existence, not value. The API redacts the webhook URL and the probe
            header values (both are credentials — see the `Monitor` interface in
            lib/models), so these chips are the only way to confirm from the UI
            that state-change notification and probe auth are actually wired up.
          -->
          {#if detail.monitor.has_webhook}
            <span class="kindtag" title={t('monitor.webhookNote')}>
              webhook
            </span>
          {/if}
          {#if probeHeaders.length > 0}
            <span class="kindtag" title={`Probe sends: ${probeHeaders.join(', ')}`}>
              {probeHeaders.length} header{probeHeaders.length === 1 ? '' : 's'}
            </span>
          {/if}
        </div>
      </div>
        <div class="actions">
          <Button variant="secondary" loading={pausing} lockedReason={writeLock} onclick={togglePause}>
            {detail.monitor.status === 'paused' ? 'Resume' : 'Pause'}
          </Button>
          <Button variant="danger" lockedReason={writeLock} onclick={() => (confirmOpen = true)}>
            {t('common.delete')}
          </Button>
        </div>
    </header>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    <StatTiles min={150}>
      <StatTile label={t('monitors.column.uptime24h')} value={fmtPct(detail.uptime.h24)} tone={pctTone(detail.uptime.h24)} />
      <StatTile label={t('monitor.stat.uptime7d')} value={fmtPct(detail.uptime.d7)} tone={pctTone(detail.uptime.d7)} />
      <StatTile label={t('monitor.stat.uptime30d')} value={fmtPct(detail.uptime.d30)} tone={pctTone(detail.uptime.d30)} />
        <div class="interval-tile">
          <span class="it-label">{t('monitors.column.interval')}</span>
          <div class="control select" class:busy={savingInterval}>
            <select
              aria-label={t('monitor.checkInterval')}
              bind:value={selectedInterval}
              disabled={savingInterval}
              use:lockTip={writeLock}
              onchange={requestIntervalChange}
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
    </StatTiles>

    <div class="section">
      <Card title={t('monitor.card.recentChecks')} padding="none">
        {#if checksLoading}
          <Skeleton rows={5} />
        {:else if checksError}
          <div class="err-banner in-card" role="alert">{checksError}</div>
        {:else if checks.length === 0}
          <EmptyState
            title={t('monitor.empty.checks')}
            description={t('monitor.empty.checksBody')}
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
              <span>{t('monitor.sort.oldest')}</span>
              <span>{barChecks.length} checks</span>
              <span>{t('monitor.sort.newest')}</span>
            </div>
          </div>

          <DataTable>
            {#snippet head()}
              <tr>
                <SortableTh key="time" sort={checkSort} onsort={onCheckSort}>{t('events.column.time')}</SortableTh>
                <SortableTh key="result" columnDefault="asc" sort={checkSort} onsort={onCheckSort}>
                  {t('monitor.column.result')}
                </SortableTh>
                <SortableTh key="code" class="num" sort={checkSort} onsort={onCheckSort}>
                  {t('monitor.column.code')}
                </SortableTh>
                <SortableTh key="latency" class="num" sort={checkSort} onsort={onCheckSort}>
                  {t('monitors.column.latency')}
                </SortableTh>
                <!-- Free text, often a whole stack of it, and blank on every
                     healthy check. Ordering it would sort the log by whichever
                     failure message happens to start with the earliest letter. -->
                <th>{t('issues.stat.error')}</th>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each sortedChecks as c (c.checked_at)}
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
      <Card title={t('monitor.card.incidents')} padding="none">
        {#if sortedIncidents.length === 0}
          <EmptyState
            title={t('monitor.empty.incidents')}
            description={t('monitor.empty.incidentsBody')}
            icon="circle-check"
          />
        {:else}
          <DataTable>
            {#snippet head()}
              <tr>
                <SortableTh key="started" sort={incidentSort} onsort={onIncidentSort}>
                  {t('explore.column.started')}
                </SortableTh>
                <SortableTh key="resolved" sort={incidentSort} onsort={onIncidentSort}>
                  {t('monitor.state.resolved')}
                </SortableTh>
                <SortableTh key="duration" class="num" sort={incidentSort} onsort={onIncidentSort}>
                  {t('explore.column.duration')}
                </SortableTh>
                <SortableTh key="cause" columnDefault="asc" sort={incidentSort} onsort={onIncidentSort}>
                  {t('monitor.column.cause')}
                </SortableTh>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each sortedIncidents as i (i.id)}
                <tr>
                  <td>{formatDateTime(i.started_at)}</td>
                  <td>
                    {#if i.resolved_at}{formatDateTime(i.resolved_at)}{:else}<span class="ongoing">{t('monitor.state.ongoing')}</span>{/if}
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

<ConfirmDialog
  bind:open={confirmOpen}
  title={t('monitor.delete')}
  message={deleteMessage}
  confirmLabel={t('monitor.delete')}
  danger
  loading={deleting}
  onconfirm={remove}
  oncancel={() => (confirmOpen = false)}
/>

<ConfirmDialog
  bind:open={intervalConfirmOpen}
  title={t('monitor.changeInterval')}
  message={intervalChangeMessage}
  confirmLabel={t('monitor.changeIntervalConfirm')}
  loading={savingInterval}
  onconfirm={confirmIntervalChange}
  oncancel={cancelIntervalChange}
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
    inset-inline-end: 11px;
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
    margin-inline-end: 8px;
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
