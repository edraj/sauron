<script lang="ts">
  import { querystring, replace } from 'svelte-spa-router';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import {
    downloadAuditCsv,
    getAuditLog,
    type AuditEntry,
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
  let entries = $state<AuditEntry[]>([]);
  let facets = $state<AuditFacets>(EMPTY_FACETS);
  let nextCursor = $state<string | null>(null);
  let loading = $state(true);
  let loadingMore = $state(false);
  let error = $state<string | null>(null);
  let selected = $state<AuditEntry | null>(null);
  let exporting = $state(false);
  let exportError = $state<string | null>(null);

  const orgId = $derived(sessionStore.currentOrgId);

  /**
   * Load page one. Every filter change and the org switch funnel through here,
   * which is why it resets the cursor rather than appending — a stale cursor
   * from the previous filter set would page through the wrong stream.
   */
  async function load() {
    if (!orgId) return;
    loading = true;
    error = null;
    try {
      const page = await getAuditLog(orgId, toApiFilters(filters));
      entries = page.entries;
      facets = page.facets;
      nextCursor = page.next_cursor;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      entries = [];
      // Deliberately NOT cleared: keeping the dropdowns populated after a
      // failed fetch is what lets the user narrow the query that just failed
      // instead of having to reload the page to get their filters back.
    } finally {
      loading = false;
    }
  }

  async function loadMore() {
    if (!orgId || !nextCursor || loadingMore) return;
    loadingMore = true;
    try {
      const page = await getAuditLog(orgId, toApiFilters(filters), nextCursor);
      entries = [...entries, ...page.entries];
      nextCursor = page.next_cursor;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
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
    if (!sameFilters(fromUrl, filters)) filters = fromUrl;
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
      exportError = e instanceof Error ? e.message : String(e);
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

  /** Diff rows for the drawer, in a stable order. */
  function diffRows(entry: AuditEntry): Array<[string, unknown, unknown]> {
    return Object.entries(entry.changes ?? {})
      .map(([field, v]) => [field, v?.from, v?.to] as [string, unknown, unknown])
      .sort((a, b) => a[0].localeCompare(b[0]));
  }
</script>

<AdminShell requireProject={false}>
  <div class="head">
    <div>
      <h1>Wall of Shame</h1>
      <p class="sub">
        Every administrative action taken in
        <strong>{sessionStore.currentOrg?.name ?? 'this organization'}</strong>, newest first.
      </p>
    </div>
    <div class="actions">
      <Button
        variant="secondary"
        onclick={exportCsv}
        disabled={exporting || loading || entries.length === 0}
        title={entries.length === 0 ? 'Nothing to export with these filters' : 'Download this view as CSV'}
      >
        {exporting ? 'Preparing…' : 'Export CSV'}
      </Button>
      <RefreshButton onclick={load} loading={loading} />
    </div>
  </div>

  <Card>
    <div class="filters">
      <label>
        <span>Range</span>
        <select bind:value={filters.range}>
          {#each RANGES as r (r.key)}
            <option value={r.key}>{r.label}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Project</span>
        <select bind:value={filters.project_id}>
          <option value={null}>All projects</option>
          {#each projectOptions as p (p.id)}
            <option value={p.id}>{p.label}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>App</span>
        <select bind:value={filters.app_id}>
          <option value={null}>All apps</option>
          {#each appOptions as a (a.id)}
            <option value={a.id}>{a.label}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Environment</span>
        <select bind:value={filters.environment_id}>
          <option value={null}>All environments</option>
          {#each envOptions as e (e.id)}
            <option value={e.id}>{e.label}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Who</span>
        <select bind:value={filters.actor_id}>
          <option value={null}>Everyone</option>
          {#each actorOptions as a (a.id)}
            <option value={a.id}>{a.label}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Action</span>
        <select bind:value={filters.action}>
          <option value={null}>All actions</option>
          {#each actionOptions as a (a.label)}
            <option value={a.label}>{describeAction(a.label).label}</option>
          {/each}
        </select>
      </label>

      <!--
        A toggle rather than another dropdown: sign-in activity is a separate
        STREAM, not another value of an existing axis. Off by default — that is
        the whole reason auth events can be recorded at all without burying the
        member, role and key events this page exists to surface.
      -->
      <label class="toggle">
        <input type="checkbox" bind:checked={filters.include_auth} />
        <span>Include sign-in activity</span>
      </label>

      {#if !isDefaultFilter(filters)}
        <Button variant="ghost" onclick={clearFilters}>Clear</Button>
      {/if}
    </div>
  </Card>

  {#if exportError}
    <p class="export-error"><Icon name="triangle-alert" size={14} /> Export failed: {exportError}</p>
  {/if}

  {#if loading}
    <div class="centered"><Spinner /></div>
  {:else if error}
    <EmptyState
      title="Could not load the audit trail"
      description={error}
      icon="triangle-alert"
    />
  {:else if entries.length === 0}
    <!--
      Two genuinely different empty states. "No results" after filtering is a
      dead end unless it offers the way out; "nothing yet" must say WHEN
      recording started, or a blank page reads as data loss rather than as a
      young log.
    -->
    {#if isDefaultFilter(filters)}
      <EmptyState
        title="Nothing recorded yet"
        description="Administrative actions — creating projects, apps and environments, adding members, resetting passwords, changing roles — appear here from the moment this feature was deployed. Actions taken before then were never recorded and cannot be reconstructed."
        icon="scroll-text"
      />
    {:else}
      <EmptyState
        title="No actions match these filters"
        description="Try widening the date range or clearing a filter."
        icon="search"
      >
        {#snippet action()}
          <Button variant="secondary" onclick={clearFilters}>Clear filters</Button>
        {/snippet}
      </EmptyState>
    {/if}
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <th>When</th>
          <th>Who</th>
          <th>Action</th>
          <th>Target</th>
          <th>Where</th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each entries as entry (entry.id)}
          {@const described = describeAction(entry.action)}
          <tr class="row" onclick={() => (selected = entry)}>
            <td><TimeValue value={entry.created_at} muted /></td>
            <td class="who">{entry.actor_email || '—'}</td>
            <td>
              <span class="action tone-{described.tone}">{described.label}</span>
              {#if entry.source === 'inspector'}
                <Badge tone="neutral" size="sm">privacy</Badge>
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
        <Button variant="secondary" onclick={loadMore} disabled={loadingMore}>
          {loadingMore ? 'Loading…' : 'Load more'}
        </Button>
      </div>
    {/if}
  {/if}
</AdminShell>

{#if selected}
  {@const entry = selected}
  {@const described = describeAction(entry.action)}
  <Modal open title={described.label} onclose={() => (selected = null)}>
    <dl class="detail">
      <dt>When</dt>
      <dd><TimeValue value={entry.created_at} asText /></dd>

      <dt>Who</dt>
      <dd>{entry.actor_email || 'unknown'}</dd>

      <dt>Action</dt>
      <dd><code>{entry.action}</code></dd>

      <dt>Target</dt>
      <dd>{entry.entity_name || '—'}</dd>

      {#if describeScope(entry)}
        <dt>Where</dt>
        <dd>{describeScope(entry)}</dd>
      {/if}
    </dl>

    {#if diffRows(entry).length > 0}
      <h3>
        {entry.source === 'inspector' ? 'Details' : 'What changed'}
      </h3>
      <DataTable>
        {#snippet head()}
          <tr>
            <th>Field</th>
            <th>Before</th>
            <th>After</th>
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
          Recorded by the PII inspector. The Privacy page has the full detail.
        {:else}
          This action has no before/after values — either nothing comparable
          changed, or the values are credentials that are deliberately never
          recorded.
        {/if}
      </p>
    {/if}
  </Modal>
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0;
    font-size: 1.35rem;
  }
  .sub {
    margin: 0.25rem 0 0;
    color: var(--text-muted);
    font-size: 0.9rem;
  }

  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 0.75rem;
  }
  .filters label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .filters select {
    min-width: 10rem;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface);
    color: var(--text);
    font-size: 0.85rem;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .toggle {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.8rem;
    color: var(--text);
    padding-bottom: 0.4rem;
  }

  .export-error {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--danger);
    font-size: 0.85rem;
    margin: 0.75rem 0 0;
  }

  .centered {
    display: flex;
    justify-content: center;
    padding: 1.5rem 0;
  }

  .row {
    cursor: pointer;
  }
  .row:hover {
    background: var(--surface-hover);
  }

  .who,
  .target {
    font-variant-numeric: tabular-nums;
  }
  .scope {
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  /*
    Emphasis is the point of the page: in a list of two hundred rows a
    deletion and a rename must not look identical.
  */
  .action.tone-destructive {
    color: var(--danger);
    font-weight: 600;
  }
  .action.tone-credential {
    color: var(--warning);
    font-weight: 600;
  }

  .detail {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.4rem 1rem;
    margin: 0 0 1rem;
    font-size: 0.9rem;
  }
  .detail dt {
    color: var(--text-muted);
  }
  .detail dd {
    margin: 0;
  }

  h3 {
    margin: 1rem 0 0.5rem;
    font-size: 0.95rem;
  }

  .before {
    color: var(--text-muted);
    text-decoration: line-through;
  }
  .after {
    font-weight: 600;
  }

  .no-diff {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--text-muted);
    font-size: 0.85rem;
    margin: 0.5rem 0 0;
  }
</style>
