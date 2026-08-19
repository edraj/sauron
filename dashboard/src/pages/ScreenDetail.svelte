<script lang="ts">
  import { t } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
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
  // `revalidating` is deliberately not surfaced: this page has no RefreshButton
  // to spin, and the payload is replaced in place when the refresh lands.
  const view = new CachedView<ScreenDetail>();

  const detail = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  const error = $derived(view.error);

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
    return x.exception_type ?? x.message ?? `Issue ${x.issue_id.slice(0, 8)}`;
  }

  function deviceLabel(d: { family: string | null; model: string | null; device_key: string }) {
    return [d.family, d.model].filter(Boolean).join(' ') || d.device_key;
  }
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/screens')}>
    <Icon name="arrow-left" size={14} />
    {t('screens.title')}
  </button>

  {#if loading && !detail}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <EmptyState title={t('screen.error.load')} description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/screens')}>{t('screen.backToList')}</Button>
      {/snippet}
    </EmptyState>
  {:else if detail}
    <h1 class="page-title mono screen-title">{screenName}</h1>

    <StatTiles min={150}>
      <StatTile label={t('screens.column.views')} value={compactNumber(detail.stats.views)} tone="primary" />
      <StatTile label={t('users.title')} value={compactNumber(detail.stats.users)} />
      <StatTile label={t('explore.column.events')} value={compactNumber(detail.stats.events)} />
      <StatTile
        label={t('screens.column.exceptions')}
        value={compactNumber(detail.stats.exceptions)}
        tone={detail.stats.exceptions > 0 ? 'error' : 'neutral'}
      />
      <StatTile label={t('screens.column.avgDwell')} value={formatDuration(detail.stats.avg_dwell_ms)} />
      <StatTile label={t('screen.stat.totalDwell')} value={formatDuration(detail.stats.total_dwell_ms)} />
    </StatTiles>

    {#key sectionKey}
      <div class="lists">
        <CollapsibleFetchCard
          title={t('explore.column.events')}
          icon="zap"
          emptyNote="No events on this screen."
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
                <dl class="facts">
                  <div><dt>{t('screen.distinctId')}</dt><dd class="mono">{e.distinct_id}</dd></div>
                  {#if e.session_id}
                    <div><dt>{t('sessions.column.session')}</dt><dd class="mono">{e.session_id}</dd></div>
                  {/if}
                  {#if e.release}
                    <div><dt>{t('issue.field.release')}</dt><dd class="mono">{e.release}</dd></div>
                  {/if}
                </dl>
                <p class="sub">{t('events.properties')}</p>
                <KeyValueList data={e.properties} emptyLabel="No properties" />
                {#if e.tags && Object.keys(e.tags).length > 0}
                  <p class="sub">{t('ui.section.tags')}</p>
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
            emptyNote="No exceptions on this screen."
            fetcher={fetchExceptions}
            rowKey={(x) => x.id}
          >
            {#snippet row(x)}
              <SectionRow
                onopen={() => push('/issues/' + x.issue_id)}
                openLabel="Open issue"
              >
                {#snippet children()}
                  <span class="mono truncate">{exceptionLabel(x)}</span>
                  <span class="faint push"><TimeValue value={x.occurred_at} asText /></span>
                {/snippet}
                {#snippet expanded()}
                  <dl class="facts">
                    {#if x.exception_type}
                      <div><dt>{t('issue.field.type')}</dt><dd class="mono">{x.exception_type}</dd></div>
                    {/if}
                    {#if x.message}
                      <div><dt>{t('screen.message')}</dt><dd>{x.message}</dd></div>
                    {/if}
                    {#if x.culprit}
                      <div><dt>{t('screen.culprit')}</dt><dd class="mono">{x.culprit}</dd></div>
                    {/if}
                    {#if x.distinct_id}
                      <div><dt>{t('screen.distinctId')}</dt><dd class="mono">{x.distinct_id}</dd></div>
                    {/if}
                  </dl>
                {/snippet}
              </SectionRow>
            {/snippet}
          </CollapsibleFetchCard>
        {/if}

        <CollapsibleFetchCard
          title={t('devices.title')}
          icon="smartphone"
          emptyNote="No devices seen on this screen."
          fetcher={fetchDevices}
          rowKey={(d) => d.device_key}
        >
          {#snippet row(d)}
            <SectionRow
              onopen={() => push('/devices/' + encodeURIComponent(d.device_key))}
              openLabel="Open device"
            >
              {#snippet children()}
                <span class="truncate">{deviceLabel(d)}</span>
                <span class="faint push">
                  <TimeValue value={d.last_seen_on_screen} asText />
                </span>
              {/snippet}
              {#snippet expanded()}
                <dl class="facts">
                  <div><dt>{t('screen.deviceKey')}</dt><dd class="mono">{d.device_key}</dd></div>
                  {#if d.os_name}
                    <div>
                      <dt>OS</dt>
                      <dd>{d.os_name}{d.os_version ? ' ' + d.os_version : ''}</dd>
                    </div>
                  {/if}
                  {#if d.arch}<div><dt>{t('device.field.arch')}</dt><dd>{d.arch}</dd></div>{/if}
                  {#if d.browser}<div><dt>{t('device.field.browser')}</dt><dd>{d.browser}</dd></div>{/if}
                  <div><dt>{t('screen.viewsHere')}</dt><dd>{compactNumber(d.views_on_screen)}</dd></div>
                  <div><dt>{t('screen.eventsHere')}</dt><dd>{compactNumber(d.events_on_screen)}</dd></div>
                  <div>
                    <dt>{t('screen.exceptionsHere')}</dt>
                    <dd>{compactNumber(d.exceptions_on_screen)}</dd>
                  </div>
                  <div>
                    <dt>{t('screen.firstSeenHere')}</dt>
                    <dd><TimeValue value={d.first_seen_on_screen} /></dd>
                  </div>
                </dl>
              {/snippet}
            </SectionRow>
          {/snippet}
        </CollapsibleFetchCard>

        <CollapsibleFetchCard
          title={t('users.title')}
          icon="users"
          emptyNote="No users seen on this screen."
          fetcher={fetchUsers}
          rowKey={(u) => u.distinct_id}
        >
          {#snippet row(u)}
            <SectionRow
              onopen={() => push('/persons/' + encodeURIComponent(u.distinct_id))}
              openLabel="Open user"
            >
              {#snippet children()}
                <span class="mono truncate">{u.distinct_id}</span>
                <span class="faint push">
                  <TimeValue value={u.last_seen_on_screen} asText />
                </span>
              {/snippet}
              {#snippet expanded()}
                <dl class="facts">
                  <div><dt>{t('screen.viewsHere')}</dt><dd>{compactNumber(u.views_on_screen)}</dd></div>
                  <div><dt>{t('screen.eventsHere')}</dt><dd>{compactNumber(u.events_on_screen)}</dd></div>
                  <div>
                    <dt>{t('screen.exceptionsHere')}</dt>
                    <dd>{compactNumber(u.exceptions_on_screen)}</dd>
                  </div>
                  <div>
                    <dt>{t('screen.firstSeenHere')}</dt>
                    <dd><TimeValue value={u.first_seen_on_screen} /></dd>
                  </div>
                </dl>
                <p class="sub">{t('person.card.traits')}</p>
                <KeyValueList data={u.properties} emptyLabel="No traits" />
              {/snippet}
            </SectionRow>
          {/snippet}
        </CollapsibleFetchCard>
      </div>
    {/key}
  {:else}
    <EmptyState
      title={t('screen.notFound')}
      description={t('screen.empty.body')}
      icon="layout-panel-top"
    />
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
  .screen-title {
    word-break: break-word;
    margin-bottom: 18px;
  }
  .lists {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
    margin-top: 20px;
    align-items: start;
  }
  .truncate {
    display: inline-block;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 12.5px;
  }
  /* Pushes the timestamp to the row's trailing edge, so the times line up in a
     column regardless of how long each row's leading label is. */
  .push {
    margin-inline-start: auto;
  }
  .facts {
    display: grid;
    gap: 4px;
    margin: 0 0 10px;
  }
  .facts div {
    display: flex;
    gap: 8px;
    font-size: 12.5px;
  }
  .facts dt {
    color: var(--text-muted);
    min-width: 110px;
    flex: none;
  }
  .facts dd {
    margin: 0;
    word-break: break-word;
  }
  .sub {
    font-size: 11.5px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-faint);
    margin: 10px 0 4px;
  }
  .faint {
    font-size: 12px;
    color: var(--text-faint);
    white-space: nowrap;
  }

  @media (max-width: 900px) {
    .lists {
      grid-template-columns: 1fr;
    }
  }
</style>
