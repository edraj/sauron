<script lang="ts">
  import { t } from '../lib/i18n';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { push } from 'svelte-spa-router';
  import { formatNumber } from '../lib/i18n';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import FactList from '../lib/components/FactList.svelte';
  import Fact from '../lib/components/Fact.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { getScreenDetail } from '../lib/api/screens';
  import {
    listScreenDevices,
    listScreenEvents,
    listScreenExceptions,
    listScreenUsers,
  } from '../lib/api/screen-sections';
  import CollapsibleFetchCard from '../lib/components/CollapsibleFetchCard.svelte';
  import SectionRow from '../lib/components/SectionRow.svelte';
  import KeyValueList from '../lib/components/KeyValueList.svelte';
  import { compactNumber, formatDuration } from '../lib/utils/format';
  import type { ScreenDetail } from '../lib/models';
  import Freshness from '../lib/components/ui/Freshness.svelte';

  interface Props {
    params?: { name?: string };
  }
  let { params }: Props = $props();

  const screenName = $derived(decodeURIComponent(params?.name ?? ''));

  // Cached view (lib/stores/cached-view.svelte.ts): a screen visited a moment ago
  // paints instantly on return and refreshes behind the rendered page instead of
  // blanking to a spinner. Re-exposed under the names the template already used,
  // so the markup is unchanged.
  //
  // `revalidating` now IS surfaced: the page grew a RefreshButton, and a
  // refresh that spun nothing while the payload swapped underneath read as the
  // click having done nothing.
  const view = new CachedView<ScreenDetail>();

  const refresher = pageRefresher(() => view.reload());

  const detail = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  const error = $derived(view.error);
  /**
   * Nothing to show and nothing failing — or the server saying 404. Either is
   * "this screen isn't here", which gets the way back rather than a Retry.
   */
  const notFound = $derived(
    !loading && !detail && (!error || view.errorStatus === 404),
  );

  // `scopeKey` belongs in the key: it carries the selected environment, which the
  // axios interceptor adds to the request but which appears in none of these
  // arguments. Omit it and one environment's screen would be served as another's.
  //
  // `force` bypasses the fresh-window short-circuit, for a call site that means
  // "go to the network now".
  async function load(appId: string, name: string, force = false) {
    await view.load(
      viewKey('screens.detail', appId, sessionStore.scopeKey, name),
      () => getScreenDetail(appId, name),
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const name = screenName;
    if (aid && name) void load(aid, name);
  });

  // -------------------------------------------------------------------------
  // The four fetch-on-demand sections.
  //
  // Each `fetcher` closes over `screenName` and `sessionStore.currentAppId`
  // rather than receiving them as props, so a card that somehow outlived a
  // screen change would request the NEW screen rather than silently re-serving
  // the old one. That is a second line of defence: the `{#key}` block in the
  // markup already destroys and rebuilds all four whenever the screen or the
  // environment changes.
  //
  // The `{#key}` is load-bearing, not decorative. `svelte-spa-router` REUSES
  // this component instance across `#/screens/A` -> `#/screens/B`, so every
  // piece of card state — rows, offset, expanded row, the collapsed flag —
  // would otherwise survive the navigation and render screen A's rows beneath
  // screen B's title and stat tiles. A test harness that mounts the page fresh
  // per case cannot observe this; only an in-place navigation can.
  const appId = $derived(sessionStore.currentAppId ?? '');

  // `scopeKey` participates for the same reason it does in `load()`: the
  // environment reaches the wire through the axios interceptor, so nothing in
  // these arguments changes when it does, and already-fetched cards would keep
  // showing another environment's rows.
  const sectionKey = $derived(`${appId}:${sessionStore.scopeKey}:${screenName}`);

  const fetchEvents = (offset: number, limit: number) =>
    listScreenEvents(appId, { name: screenName, limit, offset });
  const fetchExceptions = (offset: number, limit: number) =>
    listScreenExceptions(appId, { name: screenName, limit, offset });
  const fetchDevices = (offset: number, limit: number) =>
    listScreenDevices(appId, { name: screenName, limit, offset });
  const fetchUsers = (offset: number, limit: number) =>
    listScreenUsers(appId, { name: screenName, limit, offset });

  // The Exceptions card is hidden, not disabled, without `issue:read`: the
  // endpoint answers 200 with the bodies redacted for such a caller, so a
  // rendered card would show a list of blanks that reads as "no exceptions".
  const mayReadIssues = $derived(sessionStore.can('issue:read', { level: 'app' }));

  /**
   * A never-blank label for an exception row.
   *
   * `exception_type` and `message` are BOTH nullable, and a row where both are
   * null rendered as empty space that was still clickable — an invisible
   * control that navigates. Inherited verbatim from the static card this
   * replaced. Falls back to the issue id, which always exists.
   */
  function exceptionLabel(x: { exception_type: string | null; message: string | null; issue_id: string }) {
    return x.exception_type ?? x.message ?? t('screen.issueFallback', { id: x.issue_id.slice(0, 8) });
  }

  function deviceLabel(d: { family: string | null; model: string | null; device_key: string }) {
    return [d.family, d.model].filter(Boolean).join(' ') || d.device_key;
  }
</script>

  <!-- The heading is the name in the URL, so it is on screen before the stats
       request returns. -->
  <PageHeader
    title={screenName}
    titleClass="mono"
    back={{ label: t('nav.screens'), href: '#/screens' }}
  >
    {#snippet meta()}
      <CopyButton value={screenName} size="sm" />
    {/snippet}
    {#snippet freshness()}
      {#if detail}<Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />{/if}
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || view.revalidating} />
    {/snippet}
  </PageHeader>

  <!-- Only the stat tiles wait on the stats request. The four lists below
       fetch on demand and never depended on it — so neither is held behind
       it. -->
  {#if notFound}
    <EmptyState
      title={t('screen.notFound')}
      description={t('screen.empty.body')}
      icon="layout-panel-top"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/screens')}>{t('screen.backToList')}</Button>
      {/snippet}
    </EmptyState>
  {:else if error && !(loading && !detail)}
    <ErrorState
      title={t('screen.error.load')}
      error={view}
      onretry={() => sessionStore.currentAppId && load(sessionStore.currentAppId, screenName, true)}
    />
  {:else}
    {@const stats = detail?.stats}
    <StatTiles min={150}>
      <StatTile
        label={t('screens.column.views')}
        value={stats ? compactNumber(stats.views) : ''}
        loading={!stats}
      />
      <StatTile
        label={t('users.title')}
        value={stats ? compactNumber(stats.users) : ''}
        loading={!stats}
      />
      <StatTile
        label={t('explore.column.events')}
        value={stats ? compactNumber(stats.events) : ''}
        loading={!stats}
      />
      <StatTile
        label={t('screens.column.exceptions')}
        value={stats ? compactNumber(stats.exceptions) : ''}
        tone={stats && stats.exceptions > 0 ? 'error' : 'neutral'}
        loading={!stats}
      />
      <StatTile
        label={t('screens.column.avgDwell')}
        value={stats ? formatDuration(stats.avg_dwell_ms) : ''}
        loading={!stats}
      />
      <StatTile
        label={t('screen.stat.totalDwell')}
        value={stats ? formatDuration(stats.total_dwell_ms) : ''}
        loading={!stats}
      />
    </StatTiles>

    {#key sectionKey}
      <div class="lists">
        <CollapsibleFetchCard
          title={t('explore.column.events')}
          icon="zap"
          emptyNote={t('screen.empty.events')}
          fetcher={fetchEvents}
          rowKey={(e) => e.id}
        >
          {#snippet row(e)}
            <SectionRow>
              {#snippet children()}
                <span class="mono truncate">{e.name}</span>
                <span class="faint push"><TimeValue value={e.occurred_at} asText /></span>
              {/snippet}
              {#snippet expanded()}
                <div class="facts">
                  <FactList>
                    <Fact label={t('screen.distinctId')} mono>{e.distinct_id}</Fact>
                    {#if e.session_id}
                      <Fact label={t('sessions.column.session')} mono>{e.session_id}</Fact>
                    {/if}
                    {#if e.release}
                      <Fact label={t('issue.field.release')} mono>{e.release}</Fact>
                    {/if}
                  </FactList>
                </div>
                <p class="section-label sub">{t('events.properties')}</p>
                <KeyValueList data={e.properties} emptyLabel={t('screen.empty.properties')} />
                {#if e.tags && Object.keys(e.tags).length > 0}
                  <p class="section-label sub">{t('ui.section.tags')}</p>
                  <KeyValueList data={e.tags} />
                {/if}
              {/snippet}
            </SectionRow>
          {/snippet}
        </CollapsibleFetchCard>

        {#if mayReadIssues}
          <CollapsibleFetchCard
            title={t('screens.column.exceptions')}
            icon="triangle-alert"
            emptyNote={t('screen.empty.exceptions')}
            fetcher={fetchExceptions}
            rowKey={(x) => x.id}
          >
            {#snippet row(x)}
              <SectionRow
                onopen={() => push('/issues/' + x.issue_id)}
                openLabel={t('screen.open.issue')}
              >
                {#snippet children()}
                  <span class="mono truncate">{exceptionLabel(x)}</span>
                  <span class="faint push"><TimeValue value={x.occurred_at} asText /></span>
                {/snippet}
                {#snippet expanded()}
                  <div class="facts">
                    <FactList>
                      {#if x.exception_type}
                        <Fact label={t('issue.field.type')} mono>{x.exception_type}</Fact>
                      {/if}
                      {#if x.message}
                        <Fact label={t('screen.message')}>{x.message}</Fact>
                      {/if}
                      {#if x.culprit}
                        <Fact label={t('screen.culprit')} mono>{x.culprit}</Fact>
                      {/if}
                      {#if x.distinct_id}
                        <Fact label={t('screen.distinctId')} mono>{x.distinct_id}</Fact>
                      {/if}
                    </FactList>
                  </div>
                {/snippet}
              </SectionRow>
            {/snippet}
          </CollapsibleFetchCard>
        {/if}

        <CollapsibleFetchCard
          title={t('devices.title')}
          icon="smartphone"
          emptyNote={t('screen.empty.devices')}
          fetcher={fetchDevices}
          rowKey={(d) => d.device_key}
        >
          {#snippet row(d)}
            <SectionRow
              onopen={() => push('/devices/' + encodeURIComponent(d.device_key))}
              openLabel={t('screen.open.device')}
            >
              {#snippet children()}
                <span class="truncate">{deviceLabel(d)}</span>
                <span class="faint push">
                  <TimeValue value={d.last_seen_on_screen} asText />
                </span>
              {/snippet}
              {#snippet expanded()}
                <div class="facts">
                  <FactList>
                    <Fact label={t('screen.deviceKey')} mono>{d.device_key}</Fact>
                    {#if d.os_name}
                      <Fact label={t('device.field.os')}>
                        {d.os_name}{d.os_version ? ' ' + d.os_version : ''}
                      </Fact>
                    {/if}
                    {#if d.arch}<Fact label={t('device.field.arch')}>{d.arch}</Fact>{/if}
                    {#if d.browser}<Fact label={t('device.field.browser')}>{d.browser}</Fact>{/if}
                    <Fact label={t('screen.viewsHere')}>{formatNumber(d.views_on_screen)}</Fact>
                    <Fact label={t('screen.eventsHere')}>{formatNumber(d.events_on_screen)}</Fact>
                    <Fact label={t('screen.exceptionsHere')}>{formatNumber(d.exceptions_on_screen)}</Fact>
                    <Fact label={t('screen.firstSeenHere')}>
                      <TimeValue value={d.first_seen_on_screen} />
                    </Fact>
                  </FactList>
                </div>
              {/snippet}
            </SectionRow>
          {/snippet}
        </CollapsibleFetchCard>

        <CollapsibleFetchCard
          title={t('users.title')}
          icon="users"
          emptyNote={t('screen.empty.users')}
          fetcher={fetchUsers}
          rowKey={(u) => u.distinct_id}
        >
          {#snippet row(u)}
            <SectionRow
              onopen={() => push('/persons/' + encodeURIComponent(u.distinct_id))}
              openLabel={t('screen.open.user')}
            >
              {#snippet children()}
                <span class="mono truncate">{u.distinct_id}</span>
                <span class="faint push">
                  <TimeValue value={u.last_seen_on_screen} asText />
                </span>
              {/snippet}
              {#snippet expanded()}
                <div class="facts">
                  <FactList>
                    <Fact label={t('screen.viewsHere')}>{formatNumber(u.views_on_screen)}</Fact>
                    <Fact label={t('screen.eventsHere')}>{formatNumber(u.events_on_screen)}</Fact>
                    <Fact label={t('screen.exceptionsHere')}>{formatNumber(u.exceptions_on_screen)}</Fact>
                    <Fact label={t('screen.firstSeenHere')}>
                      <TimeValue value={u.first_seen_on_screen} />
                    </Fact>
                  </FactList>
                </div>
                <p class="section-label sub">{t('person.card.traits')}</p>
                <KeyValueList data={u.properties} emptyLabel={t('person.noTraits')} />
              {/snippet}
            </SectionRow>
          {/snippet}
        </CollapsibleFetchCard>
      </div>
    {/key}
  {/if}

<style>
  .lists {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--page-gap);
    margin-top: var(--page-gap);
    align-items: start;
  }
  .truncate {
    display: inline-block;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: var(--text-xs);
  }
  /* Pushes the timestamp to the row's trailing edge, so the times line up in a
     column regardless of how long each row's leading label is. */
  .push {
    margin-inline-start: auto;
  }
  .facts {
    margin: 0 0 10px;
  }
  .sub {
    margin: 10px 0 4px;
  }
  .faint {
    font-size: var(--text-xs);
    color: var(--text-faint);
    white-space: nowrap;
  }

  @media (max-width: 900px) {
    .lists {
      grid-template-columns: 1fr;
    }
  }
</style>
