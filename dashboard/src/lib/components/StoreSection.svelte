<script lang="ts">
  import { t } from '../i18n';
  import Card from './ui/Card.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import StatTiles from './StatTiles.svelte';
  import StatTile from './StatTile.svelte';
  import StoreInstallsChart from './StoreInstallsChart.svelte';
  import { CachedView } from '../stores/cached-view.svelte';
  import { viewKey } from '../stores/view-cache';
  import { sessionStore } from '../stores/session.svelte';
  import { getStoreMetrics, type StoreMetrics } from '../api/stores';
  import { rangeTotals } from './stores';
  import { compactNumber, relativeTime } from '../utils/format';

  interface Props {
    appId: string;
    sinceDays: number;
  }

  let { appId, sinceDays }: Props = $props();

  // Its own view, loaded independently of Overview's other five sections: a
  // store outage or a 403 here must not blank the rest of the page.
  const view = new CachedView<StoreMetrics>();
  const data = $derived(view.data ?? null);
  const series = $derived(data?.series ?? []);
  const totals = $derived(rangeTotals(series));

  const pendingCount = $derived(data?.pending_days.length ?? 0);
  const applePending = $derived(
    (data?.stores ?? []).some((s) => s.store === 'app_store' && s.state === 'pending'),
  );
  const errored = $derived((data?.stores ?? []).filter((s) => s.last_error));
  const lastSynced = $derived(
    (data?.stores ?? [])
      .map((s) => s.last_synced_at)
      .filter((v): v is string => !!v)
      .sort()
      .pop() ?? null,
  );

  // `scopeKey` is in the key even though the endpoint takes no environment
  // argument: it carries the selected environment, and this section only
  // renders in one of them. Omitting it would serve one scope's answer under
  // another's.
  $effect(() => {
    const key = viewKey('overview.stores', appId, sessionStore.scopeKey, sinceDays);
    void view.load(key, () => getStoreMetrics(appId, sinceDays));
  });

  function storeLabel(store: string): string {
    return store === 'google_play' ? 'Google Play' : 'App Store';
  }
</script>

<Card title={t('ui.store.title')}>
  {#if view.loading && !data}
    <Skeleton rows={4} height="46px" />
  {:else if view.error && !data}
    <p class="err">{view.error}</p>
  {:else}
    <StatTiles>
      <StatTile label={t('ui.store.installs')} value={compactNumber(totals.installs)} tone="success" />
      <StatTile label={t('ui.store.uninstalls')} value={compactNumber(totals.uninstalls)} tone="warning" />
      <StatTile
        label={t('ui.store.netChange')}
        value={`${totals.net >= 0 ? '+' : '−'}${compactNumber(Math.abs(totals.net))}`}
        tone={totals.net >= 0 ? 'success' : 'error'}
      />
    </StatTiles>

    <div class="chart-wrap">
      <StoreInstallsChart data={series} />
    </div>

    <div class="notes">
      {#if lastSynced}
        <!-- Stated explicitly so day-old data is never mistaken for live data:
             store reports land 1-3 days late by design. -->
        <p class="muted">Last synced {relativeTime(lastSynced)}.</p>
      {/if}
      {#if applePending}
        <p class="muted">
          {t('ui.store.preparing')}
        </p>
      {/if}
      {#if pendingCount > 0}
        <!-- NOT drawn as zero bars: the store has not published these days, and
             a zero would assert that nobody installed the app. -->
        <p class="muted">
          {pendingCount}
          {pendingCount === 1 ? 'day' : 'days'} not yet published by the store.
        </p>
      {/if}
      {#each errored as s (s.store)}
        <p class="err">{storeLabel(s.store)}: {s.last_error}</p>
      {/each}
    </div>
  {/if}
</Card>

<style>
  .chart-wrap {
    margin-top: 16px;
  }
  .notes {
    margin-top: 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .notes p {
    font-size: 12.5px;
    margin: 0;
  }
  .err {
    color: var(--error);
    font-size: 13px;
  }
</style>
