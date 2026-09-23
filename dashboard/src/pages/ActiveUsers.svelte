<script lang="ts">
  import { t } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import Sparkline from '../lib/components/Sparkline.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { envelopeStatus } from '../lib/models/freshness';
  import type { ViewEnvelope } from '../lib/api/overview';
  import { viewKey } from '../lib/stores/view-cache';
  import { downloadActiveUsersCsv, getActiveUsers } from '../lib/api/activeUsers';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber } from '../lib/utils/format';
  import {
    defaultWindow,
    encodeSelection,
    selectionCount,
    utcDayLabel,
    validateSelection,
    type AppEnvSelection,
  } from '../lib/models/active-users';
  import type { ActiveUsersReport } from '../lib/models';

  const RANGES = [
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  // Hydrate from the URL ONCE, at init — not inside an effect, so this never
  // re-runs and never fights the sync effect below. House pattern from
  // `Issues.svelte`.
  const initial = new URLSearchParams($querystring ?? '');
  const initialWindow = defaultWindow(30, new Date());
  let from = $state(initial.get('from') ?? initialWindow.from);
  let to = $state(initial.get('to') ?? initialWindow.to);

  /**
   * The app and environment come from the top bar, like every other telemetry
   * page — this page used to carry its own multi-app picker, which duplicated
   * the org/project/app/release/environment switcher and let the two disagree.
   *
   * Encoded in the wire's own vocabulary: `sessionStore.currentEnvId` is
   * `null` for all environments and the literal `'none'` for unattributed,
   * which are exactly the `EnvChoice` tokens `encodeSelection` emits. Empty
   * while no app is selected, which the empty state below explains.
   */
  const selection = $derived<AppEnvSelection>(
    sessionStore.currentAppId
      ? { [sessionStore.currentAppId]: sessionStore.currentEnvId ?? 'all' }
      : {},
  );

  // Cached view (lib/stores/cached-view.svelte.ts): the cached report paints
  // instantly on return, then refreshes behind it. Re-exposed under the names the
  // template already used, so the markup is unchanged apart from the spinner prop.
  const view = new CachedView<ViewEnvelope<ActiveUsersReport>>();

  // Two unwraps: the CachedView holds the ENVELOPE, and the envelope holds the
  // report — which is null while the server is still computing it.
  const envelope = $derived(view.data ?? null);
  const report = $derived(envelope?.data ?? null);
  /**
   * The server is building the report and has answered immediately to say so.
   *
   * Not an error and not an empty result: `computing` is a normal 200. It is
   * what stops a cold read occupying a request until the timeout turns it into
   * a 503, which is what this page was reported for.
   */
  /**
   * How to read the envelope — see `envelopeStatus`, which is where the rule
   * lives and is tested.
   *
   * A failed server-side recompute arrives as HTTP 200 with
   * `{state:"computing", data:null, error:"…"}`. Read through `view.error`
   * alone that is a success with nothing in it, and this page polls forever on
   * a skeleton while the reason sits unread in the payload.
   */
  const status = $derived(
    envelopeStatus({
      state: envelope?.state,
      error: envelope?.error,
      hasData: report !== null,
      viewError: view.error,
    }),
  );
  const computing = $derived(status.computing);
  const revalidating = $derived(view.revalidating);
  const error = $derived(status.error);

  let refreshing = $state(false);
  let exporting = $state(false);

  const selectionValid = $derived(validateSelection(selection));

  // `CachedView` starts out `loading: true`, but this page legitimately sits with
  // nothing selected and no request in flight — and `reset()` returns it to that
  // state when the last app is unticked. Gate on the same condition the load
  // effect uses (`encodeSelection` emits exactly one token per selected app), or
  // an empty selection renders a spinner that never resolves instead of the
  // "Pick an app to begin" empty state below it.
  const hasSelection = $derived(selectionCount(selection) > 0);
  // `computing` counts as loading: the request succeeded, but there is still
  // nothing to render, and the skeleton is the honest thing to show.
  const loading = $derived(hasSelection && (view.loading || (computing && !report)));

  const rangeDays = $derived(
    Math.max(1, Math.round((Date.parse(to) - Date.parse(from)) / 86_400_000)),
  );

  function setRange(days: number) {
    const w = defaultWindow(days, new Date());
    from = w.from;
    to = w.to;
  }

  /**
   * `force` bypasses the fresh-window short-circuit: an explicit Refresh click
   * means "go to the network now", cache or not.
   *
   * `scopeKey` is in the key per the house rule for every cached view: it carries
   * the selected environment, which the axios interceptor can add to a request
   * without it appearing in any caller argument. Here the `selection` tokens
   * already name each app's environment explicitly, so it is belt-and-braces
   * rather than the sole guard — but deciding that per page, per endpoint, is
   * exactly how one environment's numbers end up served as another's.
   */
  async function load(
    projectId: string,
    params: { from: string; to: string; selection: string[] },
    force = false,
  ) {
    await view.load(
      viewKey(
        'active-users.report',
        projectId,
        sessionStore.scopeKey,
        params.from,
        params.to,
        params.selection,
      ),
      () => getActiveUsers(projectId, params),
      force,
    );
  }

  /**
   * Poll while the server computes.
   *
   * Overview gets its recomputes pushed over SSE; this route has none, so the
   * page asks again until the report lands. `force` is required — without it
   * the client cache would answer from its own fresh window and the page would
   * sit on `computing` forever, having stopped asking.
   */
  $effect(() => {
    if (!status.shouldPoll) return;
    const pid = sessionStore.currentProjectId;
    if (!pid) return;
    const params = { from, to, selection: encodeSelection(selection) };
    const id = setTimeout(() => void load(pid, params, true), 1500);
    return () => clearTimeout(id);
  });

  async function refresh() {
    const pid = sessionStore.currentProjectId;
    if (!pid || !selectionValid.ok) return;
    refreshing = true;
    try {
      await load(pid, { from, to, selection: encodeSelection(selection) }, true);
    } finally {
      refreshing = false;
    }
  }

  // Wraps this page's OWN refresh rather than replacing it: the body below
  // does page-specific work a generic sweep would drop. `pageRefresher` adds
  // the server force window and the wait for the recompute to land, so a page
  // button and the one in the top bar now mean the same thing.
  const refresher = pageRefresher(refresh);

  async function exportCsv() {
    const pid = sessionStore.currentProjectId;
    const rep = report;
    if (!pid || !rep) return;
    exporting = true;
    try {
      await downloadActiveUsersCsv(
        pid,
        { from, to, selection: encodeSelection(selection) },
        rep.effective,
      );
      toastStore.success('Export downloaded.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      exporting = false;
    }
  }

  // One effect that both writes the URL and reloads, so the shareable link and
  // the displayed numbers can never describe different requests. Only the
  // window is in the URL: the app and environment are the top bar's, which the
  // session store already persists, so a shared link opens on the viewer's own
  // scope like every other page.
  $effect(() => {
    const pid = sessionStore.currentProjectId;
    const encoded = encodeSelection(selection);
    const f = from;
    const end = to;
    if (!pid) return;
    const p = new URLSearchParams();
    p.set('from', f);
    p.set('to', end);
    void replace(`/active-users?${p.toString()}`);
    if (encoded.length === 0) {
      // `reset()`, not just "show nothing": it also bumps the generation, so a
      // response for the selection the user just cleared cannot land afterwards
      // and repopulate the page.
      view.reset();
      return;
    }
    void load(pid, { from: f, to: end, selection: encoded });
  });

  const chartData = $derived(
    (report?.series ?? []).map((p) => ({
      bucket: p.day,
      count: p.active_total,
      segments: [
        { count: p.active_guest, color: 'color-mix(in srgb, var(--primary) 35%, transparent)', label: 'Guests' },
        { count: p.active_identified, color: 'var(--primary)', label: 'Identified' }
      ]
    })),
  );
  const identifiedSeries = $derived((report?.series ?? []).map((p) => p.active_identified));
  const guestSeries = $derived((report?.series ?? []).map((p) => p.active_guest));
  const peakDay = $derived(
    report && report.series.length > 0
      ? report.series.reduce((prev, curr) => (curr.active_total > prev.active_total ? curr : prev))
      : null,
  );
  const peak = $derived(peakDay?.active_total ?? null);

  function rangeLabel(): string {
    if (!report) return '';
    return `${report.effective.from.slice(0, 10)} → ${report.effective.to.slice(0, 10)}`;
  }
</script>

  <div class="active-users">
    <header class="head">
      <div>
        <h1 class="page-title">{t('activeUsers.title')}</h1>
        <!-- The caveat that qualifies the identified number belongs beside the
             number too (see the Identified tile) — a caveat one scroll away
             gets read after the figure has already been believed. -->
        <p class="sub muted">
          {t('activeUsers.subtitle')}
        </p>
      </div>
      <div class="controls">
        <div class="ranges">
          {#each RANGES as r (r.days)}
            <button
              class="range"
              class:active={rangeDays === r.days}
              onclick={() => setRange(r.days)}
            >
              {r.label}
            </button>
          {/each}
        </div>
        <!--
          Spins for a background revalidate too, not just an explicit click: that
          spinner IS the "showing cached numbers, fetching fresh" hint.
        -->
        <Freshness
            computedAt={envelope?.computed_at ?? null}
            fetchedAt={view.fetchedAt}
            revalidating={view.revalidating || computing}
          />
        <RefreshButton
          onclick={refresher.run}
          loading={refresher.busy || refreshing || revalidating}
          title={revalidating ? 'Refreshing…' : 'Refresh'}
        />
        <Button
          variant="secondary"
          onclick={exportCsv}
          loading={exporting}
          disabled={!report || !selectionValid.ok}
        >
          <Icon name="download" size={15} />
          {t('explore.exportCsv')}
        </Button>
      </div>
    </header>

    {#if report?.truncated && report.truncation_reason}
      <!-- A persistent property of the displayed data, not a transient event,
           so a banner rather than a toast. On shipped defaults (TIER_HOT_DAYS
           30, sauron-tier on in both topologies) this fires for essentially
           every operator asking for 90 days. -->
      <div class="info-banner" role="status">
        <Icon name="info" size={15} />
        <span>{report.truncation_reason}</span>
      </div>
    {/if}

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    {#if loading && !report}
      <Skeleton rows={6} />
    {:else if !selectionValid.ok}
      <Card>
        <EmptyState
          title={t('activeUsers.empty.pickApp')}
          description={t('activeUsers.empty.pickAppBody')}
          icon="users"
        />
      </Card>
    {:else if report}
      {@const rep = report}
      <StatTiles min={150}>
        <StatTile
          label={t('activeUsers.title')}
          value={rep.latest ? compactNumber(rep.latest.active_total) : '—'}
          tone="primary"
          sub={rep.latest ? `${rep.latest.day} · ${compactNumber(rep.latest.active_identified)} identified / ${compactNumber(rep.latest.active_guest)} guests` : 'no complete day yet'}
        />
        <StatTile
          label={t('activeUsers.stat.identified')}
          value={rep.latest ? compactNumber(rep.latest.active_identified) : '—'}
          sub="matched by distinct ID"
        >
          {#snippet visual()}
            <Sparkline data={identifiedSeries} />
          {/snippet}
        </StatTile>
        <StatTile
          label={t('activeUsers.stat.guests')}
          value={rep.latest ? compactNumber(rep.latest.active_guest) : '—'}
          sub="anonymous distinct IDs"
        >
          {#snippet visual()}
            <Sparkline data={guestSeries} />
          {/snippet}
        </StatTile>
        <StatTile
          label={t('activeUsers.stat.peak')}
          value={peak === null ? '—' : compactNumber(peak)}
          sub={peakDay ? `${rangeLabel()} · ${compactNumber(peakDay.active_identified)} identified / ${compactNumber(peakDay.active_guest)} guests` : rangeLabel()}
        />
      </StatTiles>

      <Card title={t('activeUsers.card.perDay')}>
        {#if chartData.length === 0}
          <EmptyState
            title={t('activeUsers.empty.noDays')}
            description={t('activeUsers.empty.noDaysBody')}
            icon="chart-column"
          />
        {:else}
          <!-- `utcDayLabel`, not the default: the buckets are pure UTC calendar
               days, and the default renders a parsed-as-UTC Date in the
               viewer's zone. The last bar is today and is still filling; it is
               drawn anyway (dropping it would make the range shorter than the
               picker says) while the tiles read from the last complete day. -->
          <TimeSeriesChart data={chartData} label={(b) => utcDayLabel(b)} />
        {/if}
      </Card>
    {/if}
  </div>

<style>
  .active-users {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
    max-width: 62ch;
  }
  .controls {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .ranges {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .range {
    font: inherit;
    font-size: 12.5px;
    padding: 5px 10px;
    color: var(--text-muted);
    background: var(--surface);
    border: 0;
    cursor: pointer;
  }
  .range.active {
    color: var(--text);
    background: var(--surface-3);
  }
  .info-banner,
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    font-size: 13px;
    border-radius: var(--radius);
  }
  .info-banner {
    color: var(--info);
    background: color-mix(in srgb, var(--info) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--info) 38%, transparent);
  }
  .err-banner {
    color: var(--error);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 38%, transparent);
  }
</style>
