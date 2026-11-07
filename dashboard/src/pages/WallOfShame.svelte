<script lang="ts">
  import { loadErrorText } from '../lib/models/load-error';
  import { errorMessage } from '../lib/api/client';
  import { untrack } from 'svelte';
  import { t, type MessageKey } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import Select from '../lib/components/ui/Select.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import FactList from '../lib/components/FactList.svelte';
  import Fact from '../lib/components/Fact.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import {
    downloadAuditCsv,
    getAuditLog,
    type AuditEntry,
    type AuditPage,
    type AuditFacets,
  } from '../lib/api/audit';
  import {
    EMPTY_FILTERS,
    RANGES,
    describeAction,
    describeScope,
    filtersFromQuery,
    filtersToQuery,
    formatFieldName,
    formatValue,
    isDefaultFilter,
    sameFilters,
    toApiFilters,
    withSelected,
    type FilterState,
    type RangeKey,
  } from '../lib/models/audit';

  const EMPTY_FACETS: AuditFacets = {
    actors: [],
    actions: [],
    projects: [],
    apps: [],
    environments: [],
  };

  // Seeded from the ROUTER's querystring, not from `window.location.hash`.
  //
  // Reading the raw hash at component init latched whatever the address bar
  // held at that instant, which during boot is not yet the final route — a
  // deep link to `?range=30d&action=role.update` restored the action but
  // silently fell back to the default range. `$querystring` is the value the
  // router has settled on, and it is what every other page here reads
  // (ActiveUsers, DevicesInventory, ResetPassword). Hydrated once at init,
  // never inside an effect, so it cannot fight the sync effect below — the
  // same shape DevicesInventory.svelte:27 documents.
  let filters = $state<FilterState>(filtersFromQuery($querystring ?? ''));
  // Cached view (lib/stores/cached-view.svelte.ts): page one paints from cache
  // on a revisit and refreshes behind the rows. Only page one is cached —
  // cursors from "Load more" are only meaningful behind the exact page that
  // produced them, so `appended` is held outside the cache.
  const view = new CachedView<AuditPage>();
  let appended = $state<AuditEntry[]>([]);
  let appendedCursor = $state<string | null>(null);

  // Spread rather than handing the cache's own array to the template.
  const entries = $derived([...(view.data?.entries ?? []), ...appended]);
  const nextCursor = $derived(
    appended.length > 0 ? appendedCursor : (view.data?.next_cursor ?? null),
  );
  const loading = $derived(view.loading);
  const revalidating = $derived(view.revalidating);

  // Facets are mirrored into their own state rather than read off `view.data`,
  // to preserve a deliberate behaviour of the old code: a failed fetch clears
  // the rows but NOT the dropdowns, because keeping them populated is what lets
  // the user narrow the query that just failed instead of reloading the page to
  // get their filters back. `view.data` goes undefined on a cache-miss failure,
  // so reading facets straight off it would blank them.
  let facets = $state<AuditFacets>(EMPTY_FACETS);
  $effect(() => {
    const fresh = view.data?.facets;
    // Shallow copy: `view.data` is the cache's own object, and assigning it into
    // `$state` would deep-proxy it so any later write reached the cached payload.
    if (fresh) facets = { ...fresh };
  });

  let loadingMore = $state(false);
  // A "Load more" failure is reported in a notice above the rows it failed to
  // extend; only a page-one failure replaces the list (ErrorState).
  let moreError = $state<string | null>(null);
  let selected = $state<AuditEntry | null>(null);
  let exporting = $state(false);
  let exportError = $state<string | null>(null);

  const orgId = $derived(sessionStore.currentOrgId);

  /**
   * Load page one. Every filter change and the org switch funnel through here,
   * which is why it resets the cursor rather than appending — a stale cursor
   * from the previous filter set would page through the wrong stream.
   */
  async function load(force = false) {
    if (!orgId) return;
    appended = [];
    appendedCursor = null;
    moreError = null;
    const api = toApiFilters(filters);
    await view.load(
      // Keyed on `filtersToQuery(filters)` — the USER's filter set — and NOT on
      // `api`. `toApiFilters` resolves a range like `7d` into an absolute `from`
      // computed from the current clock, so it returns a different value on
      // every call: keying on it mints a new entry per load and the cache can
      // never hit. Measured before this fix — three consecutive loads of the
      // unchanged default filters produced `from=…17:33:39`, `…17:34:36` and
      // `…17:34:49`, i.e. three keys and three requests.
      //
      // The query string is already this page's canonical identity for a filter
      // set (both URL-sync effects above compare through it), so it is stable
      // across calls and carries every field the request varies on. The cost is
      // that "last 7 days" does not slide within the 60s fresh window, which is
      // exactly the staleness contract the rest of the cache runs on.
      viewKey('audit.log', orgId, filtersToQuery(filters)),
      () => getAuditLog(orgId, api),
      force,
    );
  }

  /** Refresh must reach the network, so it always forces. */
  async function refresh() {
    viewCache.invalidate('audit.log');
    await load(true);
  }

  // Wraps this page's OWN refresh rather than replacing it: the body below
  // does page-specific work a generic sweep would drop. `pageRefresher` adds
  // the server force window and the wait for the recompute to land, so a page
  // button and the one in the top bar now mean the same thing.
  const refresher = pageRefresher(refresh);

  async function loadMore() {
    const cursor = nextCursor;
    if (!orgId || !cursor || loadingMore) return;
    loadingMore = true;
    try {
      const page = await getAuditLog(orgId, toApiFilters(filters), cursor);
      appended = [...appended, ...page.entries];
      appendedCursor = page.next_cursor;
    } catch (e) {
      moreError = errorMessage(e);
    } finally {
      loadingMore = false;
    }
  }

  // Reload when the org changes or any filter moves. `filters` is read
  // through `filtersToQuery` so the effect depends on the VALUES rather than
  // the object identity — `$state` deep-proxies the object, so depending on
  // identity alone would re-run on every keystroke that touched it.
  // The router owns the URL; the page follows it. This is what makes the
  // browser Back button — which no change handler sees — restore the previous
  // filter set instead of leaving the table under a stale URL.
  //
  // Guarded with `sameFilters`, NOT a query-string comparison: key order
  // differs between a pasted URL and this page's canonical encoding, so
  // comparing strings makes this effect and the one below disagree forever
  // and reload the page in a loop.
  $effect(() => {
    const fromUrl = filtersFromQuery($querystring ?? '');
    // `untrack` on `filters` is load-bearing: this is the URL -> state
    // direction ONLY. Read reactively, this effect also re-runs when the USER
    // changes a filter — and at that moment the URL has not been rewritten yet
    // (that is the effect below), so it compares the new selection against the
    // stale URL, finds them different, and writes the URL's value back. Every
    // dropdown on the page then snaps to its previous value the instant it is
    // changed. Measured before this fix: Range 7d -> 24h reverted to 7d within
    // 600 ms, with the address bar never updating.
    if (!sameFilters(fromUrl, untrack(() => filters))) filters = fromUrl;
  });

  // Keep the address bar in step so a filtered view is linkable. `replace`,
  // not `push`: narrowing a filter is not a navigation, and pushing would make
  // Back walk every intermediate filter state instead of leaving the page.
  $effect(() => {
    const query = filtersToQuery(filters);
    if (!sameFilters(filtersFromQuery($querystring ?? ''), filters)) {
      replace(query ? `/admin/wall-of-shame?${query}` : '/admin/wall-of-shame');
    }
  });

  // Loading is its own effect, keyed on the org and the filter VALUES. Keeping
  // it out of the URL-sync effects above means a cosmetic URL rewrite can
  // never trigger a refetch.
  $effect(() => {
    filtersToQuery(filters);
    if (orgId) void load();
  });

  /**
   * Export the CURRENT filter set, not the loaded rows.
   *
   * Kept separate from `error`: a failed download must not blank the table the
   * user is looking at, which is the same split the Storage page draws between
   * load and action errors.
   */
  async function exportCsv() {
    if (!orgId || exporting) return;
    exporting = true;
    exportError = null;
    try {
      await downloadAuditCsv(orgId, toApiFilters(filters));
    } catch (e) {
      exportError = errorMessage(e);
    } finally {
      exporting = false;
    }
  }

  function clearFilters() {
    filters = { ...EMPTY_FILTERS };
  }

  // Each facet-driven select pins whatever is currently selected, so a value
  // hydrated from the URL survives the render that happens BEFORE the facets
  // for it have arrived. Without this the binding nulls itself and the page
  // loops — see `withSelected`.
  const projectOptions = $derived(withSelected(facets.projects, filters.project_id));
  const appOptions = $derived(withSelected(facets.apps, filters.app_id));
  const envOptions = $derived(withSelected(facets.environments, filters.environment_id));
  const actorOptions = $derived(withSelected(facets.actors, filters.actor_id));
  const actionOptions = $derived(
    withSelected(
      facets.actions.map((a) => ({ id: a.label, label: a.label })),
      filters.action,
    ).map((a) => ({ id: a.id, label: a.id ?? a.label })),
  );

  // `Select` speaks strings, and these filters use `null` for "any". '' is the
  // "any" option on the way in and becomes `null` again on the way out.
  type FacetFilter = 'project_id' | 'app_id' | 'environment_id' | 'actor_id' | 'action';
  function setFacet(field: FacetFilter, v: string) {
    filters[field] = v === '' ? null : v;
  }
  const anyOption = (label: string) => ({ value: '', label });
  const facetSelectOptions = (opts: Array<{ id: string | null; label: string }>) =>
    opts.filter((o) => o.id !== null).map((o) => ({ value: o.id as string, label: o.label }));

  const RANGE_LABELS: Record<RangeKey, MessageKey> = {
    '24h': 'time.range.last24h',
    '7d': 'time.range.last7d',
    '30d': 'time.range.last30d',
    '90d': 'time.range.last90d',
    all: 'purge.allTime',
  };
  const rangeOptions = $derived(RANGES.map((r) => ({ value: r.key, label: t(RANGE_LABELS[r.key]) })));

  /** A narrowing filter other than the time range. */
  const facetFiltered = $derived(!isDefaultFilter({ ...filters, range: EMPTY_FILTERS.range }));

  /** Diff rows for the drawer, in a stable order. */
  function diffRows(entry: AuditEntry): Array<[string, unknown, unknown]> {
    return Object.entries(entry.changes ?? {})
      .map(([field, v]) => [field, v?.from, v?.to] as [string, unknown, unknown])
      .sort((a, b) => a[0].localeCompare(b[0]));
  }
</script>

<AdminShell>
  <PageHeader
    title={t('admin.nav.wallOfShame')}
    subtitle={`${t('prose.audit.lede')} ${sessionStore.currentOrg?.name ?? t('members.thisOrganization')}${t('prose.audit.newestFirst')}`}
  >
    {#snippet freshness()}
      <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || loading || revalidating} />
    {/snippet}
    {#snippet actions()}
      <Button
        variant="secondary"
        onclick={exportCsv}
        loading={exporting}
        disabled={loading || entries.length === 0}
        title={entries.length === 0 ? t('audit.exportNothing') : t('audit.exportTitle')}
      >
        {t('explore.exportCsv')}
      </Button>
    {/snippet}
  </PageHeader>

  <div class="content">
    <div class="filters">
      <Select
        label={t('journeys.range')}
        value={filters.range}
        options={rangeOptions}
        onchange={(v) => (filters.range = v as RangeKey)}
      />
      <Select
        label={t('storage.column.project')}
        value={filters.project_id ?? ''}
        options={[anyOption(t('audit.filter.allProjects')), ...facetSelectOptions(projectOptions)]}
        onchange={(v) => setFacet('project_id', v)}
      />
      <Select
        label={t('nav.selectApp')}
        value={filters.app_id ?? ''}
        options={[anyOption(t('audit.filter.allApps')), ...facetSelectOptions(appOptions)]}
        onchange={(v) => setFacet('app_id', v)}
      />
      <Select
        label={t('nav.env')}
        value={filters.environment_id ?? ''}
        options={[anyOption(t('audit.filter.allEnvironments')), ...facetSelectOptions(envOptions)]}
        onchange={(v) => setFacet('environment_id', v)}
      />
      <Select
        label={t('audit.column.who')}
        value={filters.actor_id ?? ''}
        options={[anyOption(t('audit.filter.everyone')), ...facetSelectOptions(actorOptions)]}
        onchange={(v) => setFacet('actor_id', v)}
      />
      <Select
        label={t('audit.column.action')}
        value={filters.action ?? ''}
        options={[
          anyOption(t('audit.filter.allActions')),
          ...actionOptions.map((a) => ({ value: a.label, label: describeAction(a.label).label })),
        ]}
        onchange={(v) => setFacet('action', v)}
      />

      <!--
        A toggle rather than another dropdown: sign-in activity is a separate
        STREAM, not another value of an existing axis. Off by default — that is
        the whole reason auth events can be recorded at all without burying the
        member, role and key events this page exists to surface.
      -->
      <label class="toggle">
        <input type="checkbox" bind:checked={filters.include_auth} />
        <span>{t('audit.includeSignIns')}</span>
      </label>

      {#if !isDefaultFilter(filters)}
        <Button variant="ghost" onclick={clearFilters}>{t('common.clear')}</Button>
      {/if}
    </div>

    {#if exportError}
      <InlineNotice tone="error">{t('audit.exportFailed', { error: exportError })}</InlineNotice>
    {/if}
    {#if moreError}
      <InlineNotice tone="error">{moreError}</InlineNotice>
    {/if}
    {#if view.error && view.data !== undefined}
      <!-- A forced Refresh failed over rows we already had: they stay. -->
      <InlineNotice tone="warning">
        {t('ui.staleResults', { error: loadErrorText(view) })}
        {#snippet action()}
          <Button variant="ghost" size="sm" onclick={refresher.run}>{t('common.retry')}</Button>
        {/snippet}
      </InlineNotice>
    {/if}

    {#if loading}
      <Skeleton rows={8} />
    {:else if view.error && view.data === undefined}
      <ErrorState
        title={t('common.couldntLoad', { thing: t('audit.thing') })}
        error={view}
        onretry={refresher.run}
      />
    {:else if entries.length === 0}
      <!--
        Three empty states, by what the page actually knows. A narrowing filter
        is a dead end unless it offers the way out; a time window cannot claim
        nothing ever happened; and only over all time does "nothing yet" hold —
        where it must say WHEN recording started, or a blank page reads as data
        loss rather than as a young log.
      -->
      {#if facetFiltered}
        <EmptyState
          title={t('audit.empty.filtered.title')}
          description={t('audit.empty.filtered.body')}
          icon="search"
        >
          {#snippet action()}
            <Button variant="secondary" onclick={clearFilters}>{t('audit.filter.clear')}</Button>
          {/snippet}
        </EmptyState>
      {:else if filters.range !== 'all'}
        <EmptyState
          title={t('common.empty.range', { things: t('audit.things') })}
          description={t('common.empty.rangeHint')}
          icon="scroll-text"
        />
      {:else}
        <EmptyState
          title={t('audit.empty.title')}
          description={t('audit.empty.body')}
          icon="scroll-text"
        />
      {/if}
    {:else}
      <div>
        <DataTable>
          {#snippet head()}
            <tr>
              <th>{t('ui.opModal.when')}</th>
              <th>{t('audit.column.who')}</th>
              <th>{t('audit.column.action')}</th>
              <th>{t('audit.column.target')}</th>
              <th>{t('audit.column.where')}</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each entries as entry (entry.id)}
              {@const described = describeAction(entry.action)}
              <tr class="clickable" onclick={() => (selected = entry)}>
                <td><TimeValue value={entry.created_at} muted /></td>
                <td class="who">{entry.actor_email || '—'}</td>
                <td>
                  <span class="action tone-{described.tone}">{described.label}</span>
                  {#if entry.source === 'inspector'}
                    <Badge tone="neutral" size="sm">{t('audit.privacy')}</Badge>
                  {/if}
                </td>
                <td class="target">{entry.entity_name || '—'}</td>
                <td class="scope">{describeScope(entry) || '—'}</td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        {#if nextCursor}
          <div class="centered">
            <Button variant="secondary" onclick={loadMore} loading={loadingMore}>
              {t('failures.loadMore')}
            </Button>
          </div>
        {/if}
      </div>
    {/if}
  </div>
</AdminShell>

{#if selected}
  {@const entry = selected}
  {@const described = describeAction(entry.action)}
  <Modal open title={described.label} onclose={() => (selected = null)}>
    <div class="detail">
      <FactList>
        <Fact label={t('ui.opModal.when')}><TimeValue value={entry.created_at} asText /></Fact>
        <Fact label={t('audit.column.who')}>{entry.actor_email || t('common.unknown')}</Fact>
        <Fact label={t('audit.column.action')} mono>{entry.action}</Fact>
        <Fact label={t('audit.column.target')}>{entry.entity_name || '—'}</Fact>
        {#if describeScope(entry)}
          <Fact label={t('audit.column.where')}>{describeScope(entry)}</Fact>
        {/if}
      </FactList>
    </div>

    {#if diffRows(entry).length > 0}
      <h3 class="section-label diff-label">
        {entry.source === 'inspector' ? t('common.details') : t('audit.whatChanged')}
      </h3>
      <DataTable>
        {#snippet head()}
          <tr>
            <th>{t('audit.column.field')}</th>
            <th>{t('audit.filter.before')}</th>
            <th>{t('audit.filter.after')}</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each diffRows(entry) as [field, from, to] (field)}
            <tr>
              <td>{formatFieldName(field)}</td>
              <td class="before">{formatValue(from)}</td>
              <td class="after">{formatValue(to)}</td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    {:else}
      <p class="no-diff">
        <Icon name="info" size={14} />
        {#if entry.source === 'inspector'}
          {t('audit.noDiffInspector')}
        {:else}
          {t('audit.noDiff')}
        {/if}
      </p>
    {/if}
  </Modal>
{/if}

<style>
  .content {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 12px;
  }
  .filters :global(.field) {
    min-width: 160px;
  }

  .toggle {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    color: var(--text);
    height: var(--control-h);
  }

  .centered {
    display: flex;
    justify-content: center;
    padding: 24px 0;
  }

  /* .clickable handles hover in DataTable now, so .row is gone */

  .who,
  .target {
    font-variant-numeric: tabular-nums;
  }
  .scope {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }

  /*
    Emphasis is the point of the page: in a list of two hundred rows a
    deletion and a rename must not look identical.
  */
  .action.tone-destructive {
    color: var(--error);
    font-weight: var(--weight-strong);
  }
  .action.tone-credential {
    color: var(--warning);
    font-weight: var(--weight-strong);
  }

  .detail {
    margin: 0 0 16px;
  }

  .diff-label {
    margin: 16px 0 8px;
  }

  .before {
    color: var(--text-muted);
    text-decoration: line-through;
  }
  .after {
    font-weight: var(--weight-medium);
  }

  .no-diff {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 8px 0 0;
  }
</style>
