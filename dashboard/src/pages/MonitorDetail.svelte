<script lang="ts">
  import { t, formatNumber } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import {
    getMonitorSummary,
    getMonitorUptime,
    getMonitorIncidents,
    getMonitorChecks,
    updateMonitor,
    deleteMonitor,
    type MonitorSummary,
  } from '../lib/api/monitors';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { MONITOR_INTERVALS, formatInterval, intervalLabel } from '../lib/constants/monitorIntervals';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import Select from '../lib/components/ui/Select.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import type { MonitorDetail, MonitorCheck, MonitorIncident } from '../lib/models';
  import { lockedBy } from '../lib/models/page-access';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ViewSection from '../lib/components/ViewSection.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import LatencyBadge from '../lib/components/LatencyBadge.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import {
    formatDuration,
    durationBetween,
    formatPercent,
    formatTimestamp,
  } from '../lib/utils/format';
  import {
    MONITOR_CHECK_DEFAULT_SORT,
    MONITOR_INCIDENT_DEFAULT_SORT,
    monitorCheckAccessor,
    monitorIncidentAccessor,
  } from '../lib/models/monitor-detail-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';

  let { params }: { params: { id: string } } = $props();

  // Four cached views, not one: the split is deliberate (see `load`). The
  // monitor row says what the monitor IS; three uptime aggregates, an incident
  // scan and 24 h of check rows say how it has been doing. The header renders
  // the moment the row lands, and each of the others fills in — or fails —
  // inside its own block.
  const detailView = new CachedView<MonitorSummary>();
  const uptimeView = new CachedView<MonitorDetail['uptime']>();
  const incidentsView = new CachedView<MonitorIncident[]>();
  const checksView = new CachedView<MonitorCheck[]>();
  const sections = [detailView, uptimeView, incidentsView, checksView];

  // `reload()` replays the view's most recent key and fetcher with force,
  // which is exactly what Refresh means here — the page's loads are driven by
  // effects, so there is no `load(force)` to call and reconstructing the key
  // by hand would be a second definition of it, free to drift.
  const refresher = pageRefresher(async () => {
    // Every view: a refresh that updated only some of the page would be
    // misread as the rest having stopped moving.
    await Promise.all(sections.map((v) => v.reload()));
  });

  const detail = $derived(detailView.data ?? null);
  // The checks half loads independently of `detail` — see load(). It waits
  // and fails inside its own card (through `ViewSection`) instead of holding
  // up (or blanking) the whole page.
  const checks = $derived(checksView.data ?? []);
  const loading = $derived(detailView.loading);
  const revalidating = $derived(
    detailView.revalidating ||
      uptimeView.revalidating ||
      incidentsView.revalidating ||
      checksView.revalidating,
  );
  const uptime = $derived(uptimeView.data ?? null);
  const uptimeLoading = $derived(uptimeView.loading && !uptimeView.hasData);
  /**
   * The monitor read failing, or an action (pause / interval / delete) failing.
   * Kept apart because they have different lifetimes — an action's message must
   * survive a background revalidate, and a load error must not be cleared by
   * starting an unrelated action.
   */
  let actionError = $state<string | null>(null);
  const error = $derived(actionError ?? detailView.error);
  /**
   * The monitor ROW failed and left nothing to show. A 404 is "not found",
   * with the way back; anything else is a load error, with Retry. Kept apart
   * because "Monitor not found" over a timeout sent people looking for a
   * deleted monitor that was never deleted.
   */
  const pageFailed = $derived(!loading && !detail && !!detailView.error);
  const notFound = $derived(pageFailed && detailView.errorStatus === 404);
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
  //
  // A string because `Select` speaks strings; the seconds are parsed back out
  // where they are used.
  let selectedInterval = $state('60');

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
    const name = detail.monitor.name;
    if (n === 0) return t('monitor.confirm.delete', { name });
    if (n === 1) return t('monitor.confirm.deleteOneRule', { name });
    return t('monitor.confirm.deleteRules', { name, n });
  });

  const intervalChangeMessage = $derived.by(() => {
    if (!detail || pendingInterval === null) return '';
    return t('monitor.confirm.interval', {
      name: detail.monitor.name,
      from: formatInterval(detail.monitor.interval_seconds),
      to: formatInterval(pendingInterval),
    });
  });

  // `$derived` so the labels follow a language switch.
  const intervalOptions = $derived(
    MONITOR_INTERVALS.map((o) => ({ value: String(o.seconds), label: intervalLabel(o) })),
  );

  /** The probe kind as a word, not the wire value; an unknown one prints as sent. */
  function kindLabel(kind: string): string {
    if (kind === 'http') return t('monitors.http');
    if (kind === 'tcp') return t('monitor.kind.tcp');
    return kind;
  }

  /** One availability bar's tooltip: when, up or down, and how fast. */
  function barTitle(c: MonitorCheck): string {
    const parts = [
      formatTimestamp(c.checked_at),
      c.up ? t('monitor.status.up') : t('monitor.status.down'),
    ];
    if (c.response_time_ms != null) parts.push(`${c.response_time_ms} ms`);
    return parts.join(' · ');
  }

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
    sortRows(incidentsView.data ?? [], monitorIncidentAccessor(incidentSort.key), incidentSort.dir),
  );

  function onCheckSort(key: string, columnDefault: SortDir) {
    checkSort = toggleSort(checkSort, key, columnDefault);
  }
  function onIncidentSort(key: string, columnDefault: SortDir) {
    incidentSort = toggleSort(incidentSort, key, columnDefault);
  }

  async function load(force = false) {
    // All issued together (none feeds another), and not JOINED: the header,
    // config and actions need only `detail`, so they render the moment it
    // lands instead of waiting on the aggregates or 24 h of check rows — and a
    // failed section degrades to a message inside its own card rather than
    // blanking a page whose monitor half arrived fine.
    //
    // `params.id` is in every key: the router REUSES this component across
    // `#/monitors/A` -> `#/monitors/B`, so a key without it would repaint the
    // previous monitor's rows under the new id.
    const id = params.id;
    const scope = sessionStore.scopeKey;
    const rest = Promise.allSettled([
      checksView.load(viewKey('monitor.checks', id, scope), () => getMonitorChecks(id, 24), force),
      uptimeView.load(viewKey('monitor.uptime', id, scope), () => getMonitorUptime(id), force),
      incidentsView.load(
        viewKey('monitor.incidents', id, scope),
        () => getMonitorIncidents(id),
        force,
      ),
    ]);
    await detailView.load(
      viewKey('monitor.detail', id, scope),
      () => getMonitorSummary(id),
      force,
    );
    // Callers (the pause/interval/delete refreshes) await the WHOLE load, so
    // their "refresh finished" contract still covers every section.
    await rest;
  }

  /**
   * Reseed the interval control from whatever monitor is on screen.
   *
   * Was an inline assignment after the fetch, which a cache HIT never runs —
   * and this page is reused across `#/monitors/A` -> `#/monitors/B`, so the
   * control would show the interval of the monitor navigated away from. Driven
   * off the payload it is correct for a hit and a fetch alike.
   */
  $effect(() => {
    const d = detail;
    if (!d) return;
    selectedInterval = String(d.monitor.interval_seconds);
    pendingInterval = null;
  });

  async function togglePause() {
    if (!detail) return;
    pausing = true; actionError = null;
    try {
      await updateMonitor(params.id, { enabled: detail.monitor.status === 'paused' });
      // Monitors.svelte serves its list from the view cache, so the status column
      // there would keep showing the pre-toggle value — with no request in flight
      // to correct it — for the whole fresh window. Dropping the key is what makes
      // the Monitors back link refetch.
      viewCache.invalidate('monitors.list');
      await load();
    } catch (e) { actionError = (e as Error).message; }
    finally { pausing = false; }
  }

  // Picking an option no longer saves -- it only asks. Changing the interval
  // re-times every future check, so it goes through the same confirm step the
  // other two state-changing controls on this page use.
  function requestIntervalChange(value: string) {
    const seconds = Number(value);
    if (!detail || seconds === detail.monitor.interval_seconds) return;
    pendingInterval = seconds;
    intervalConfirmOpen = true;
  }

  // Also the Escape/backdrop path: ConfirmDialog forwards Modal's `onclose`
  // here, so every way out of the dialog reverts the control.
  function cancelIntervalChange() {
    intervalConfirmOpen = false;
    pendingInterval = null;
    if (detail) selectedInterval = String(detail.monitor.interval_seconds);
  }

  async function confirmIntervalChange() {
    if (!detail || pendingInterval === null) return;
    const seconds = pendingInterval;
    savingInterval = true; actionError = null;
    try {
      await updateMonitor(params.id, { interval_seconds: seconds });
      viewCache.invalidate('monitors.list');
      intervalConfirmOpen = false;
      // Clears `pendingInterval` and reseeds the control from the saved value.
      await load();
    } catch (err) {
      actionError = (err as Error).message;
      // The change did not take, so the control must not keep showing it.
      cancelIntervalChange();
    } finally {
      savingInterval = false;
    }
  }

  async function remove() {
    deleting = true; actionError = null;
    try {
      await deleteMonitor(params.id);
      // Must happen before the navigation: Monitors.svelte's effect runs on mount
      // and would otherwise hit a fresh cache entry that still contains the row we
      // just deleted, short-circuiting the network entirely.
      viewCache.invalidate('monitors.list');
      push('/monitors');
    } catch (e) {
      actionError = (e as Error).message;
      deleting = false;
      confirmOpen = false;
    }
  }

  // The API answers uptime as a percentage (99.95); `formatPercent` takes a ratio.
  const fmtPct = (v: number | null | undefined) => formatPercent(v == null ? null : v / 100, 2);
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

  <!-- The title is the id from the URL until the monitor row lands. -->
  <PageHeader
    title={detail?.monitor.name ?? params.id}
    titleClass={detail ? '' : 'mono'}
    back={{ label: t('nav.monitors'), href: '#/monitors' }}
  >
    {#snippet badge()}
      {#if detail}<StatusPill status={detail.monitor.status} />{/if}
    {/snippet}
    {#snippet meta()}
      {#if detail}
        <Badge tone="neutral" size="sm">{kindLabel(detail.monitor.kind)}</Badge>
        <span class="meta-id">
          <span class="mono">{detail.monitor.target}</span>
          <CopyButton value={detail.monitor.target} size="sm" />
        </span>
        <!--
          Existence, not value. The API redacts the webhook URL and the probe
          header values (both are credentials — see the `Monitor` interface in
          lib/models), so these chips are the only way to confirm from the UI
          that state-change notification and probe auth are actually wired up.
        -->
        {#if detail.monitor.has_webhook}
          <span class="chip-tip" title={t('monitor.webhookNote')}>
            <Badge tone="neutral" size="sm">{t('monitor.meta.webhook')}</Badge>
          </span>
        {/if}
        {#if probeHeaders.length > 0}
          <span
            class="chip-tip"
            title={t('monitor.meta.probeSends', { headers: probeHeaders.join(', ') })}
          >
            <Badge tone="neutral" size="sm">
              {t('monitor.meta.headers', { n: probeHeaders.length })}
            </Badge>
          </span>
        {/if}
      {/if}
    {/snippet}
    {#snippet freshness()}
      {#if detail}<Freshness fetchedAt={detailView.fetchedAt} {revalidating} />{/if}
    {/snippet}
    {#snippet refresh()}
      <RefreshButton
        onclick={refresher.run}
        loading={refresher.busy || detailView.revalidating || checksView.revalidating}
      />
    {/snippet}
    {#snippet actions()}
      {#if detail}
        <!-- The interval's value is in the stat tile below; this is where it
             changes. -->
        <span class="interval-ctl">
          <Select
            bind:value={selectedInterval}
            options={intervalOptions}
            ariaLabel={t('monitor.checkInterval')}
            title={t('monitor.checkInterval')}
            disabled={savingInterval}
            lockedReason={writeLock}
            onchange={requestIntervalChange}
          />
        </span>
        <Button variant="secondary" loading={pausing} lockedReason={writeLock} onclick={togglePause}>
          {detail.monitor.status === 'paused' ? t('monitor.action.resume') : t('monitor.action.pause')}
        </Button>
        <Button variant="danger" lockedReason={writeLock} onclick={() => (confirmOpen = true)}>
          {t('common.delete')}
        </Button>
      {/if}
    {/snippet}
  </PageHeader>

  <!-- The monitor ROW decides whether there is a page at all; every block below
       is a section of a page that exists, and waits and fails on its own. -->
  {#if notFound}
    <EmptyState
      title={t('monitor.notFound')}
      description={t('monitor.notFound.body')}
      icon="inbox"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/monitors')}>{t('monitor.backToMonitors')}</Button>
      {/snippet}
    </EmptyState>
  {:else if pageFailed}
    <ErrorState title={t('monitor.error.load')} error={detailView} onretry={() => load(true)} />
  {:else}
    {#if error}
      <div class="notice">
        <InlineNotice tone="error">{error}</InlineNotice>
      </div>
    {/if}

    <StatTiles min={150}>
      <!-- `?? null`: a failed uptime read renders the em dash every tile already
           uses for "no checks in this window", not a shimmer that never ends. -->
      <StatTile
        label={t('monitors.column.uptime24h')}
        value={fmtPct(uptime?.h24)}
        tone={pctTone(uptime?.h24 ?? null)}
        loading={uptimeLoading}
      />
      <StatTile
        label={t('monitor.stat.uptime7d')}
        value={fmtPct(uptime?.d7)}
        tone={pctTone(uptime?.d7 ?? null)}
        loading={uptimeLoading}
      />
      <StatTile
        label={t('monitor.stat.uptime30d')}
        value={fmtPct(uptime?.d30)}
        tone={pctTone(uptime?.d30 ?? null)}
        loading={uptimeLoading}
      />
      <StatTile
        label={t('monitors.column.interval')}
        value={detail ? formatInterval(detail.monitor.interval_seconds) : ''}
        loading={!detail}
      />
    </StatTiles>

    <div class="sections">
      <Card title={t('monitor.card.recentChecks')}>
        <ViewSection view={checksView} rows={5}>
          {#if checks.length === 0}
            <EmptyState
              size="section"
              title={t('monitor.empty.checks24h')}
              description={t('monitor.empty.checks24hBody')}
            />
          {:else}
            <div class="bar-wrap">
              <div class="uptime-bar" aria-hidden="true">
                {#each barChecks as c (c.checked_at)}
                  <span class="bar" class:down={!c.up} title={barTitle(c)}></span>
                {/each}
              </div>
              <div class="bar-legend">
                <span>{t('monitor.sort.oldest')}</span>
                <span>{t('monitor.bar.checks', { n: formatNumber(barChecks.length) })}</span>
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
                    <td><TimeValue value={c.checked_at} /></td>
                    <td><StatusPill status={c.up ? 'up' : 'down'} /></td>
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
        </ViewSection>
      </Card>

      <Card title={t('monitor.card.incidents')}>
        <ViewSection view={incidentsView} rows={4}>
          {#if sortedIncidents.length === 0}
            <EmptyState
              size="section"
              title={t('monitor.empty.incidents')}
              description={t('monitor.empty.incidentsBody')}
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
                    <td><TimeValue value={i.started_at} /></td>
                    <td>
                      {#if i.resolved_at}
                        <TimeValue value={i.resolved_at} />
                      {:else}
                        <Badge tone="warning" size="sm">{t('monitor.state.ongoing')}</Badge>
                      {/if}
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
        </ViewSection>
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
  .meta-id {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: var(--text-2xs);
    overflow-wrap: anywhere;
  }
  .chip-tip {
    display: inline-flex;
    cursor: help;
  }
  .interval-ctl {
    display: inline-flex;
  }
  .notice {
    margin-bottom: var(--page-gap);
  }
  .sections {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
    margin-top: var(--page-gap);
  }

  /* --- availability strip (signature) --------------------------------------- */
  .bar-wrap {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 4px 0 16px;
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
    font-size: var(--text-xs);
    color: var(--text-faint);
  }

  /* --- table cells ---------------------------------------------------------- */
  .errtext {
    display: inline-block;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
  }
  .cause {
    font-weight: var(--weight-medium);
    margin-inline-end: 8px;
  }
</style>
