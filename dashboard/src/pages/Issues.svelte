<script lang="ts">
  import { push, querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { ISSUE_FIELDS, encodeFilters, parseFilters, type Filter } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listIssues, getIssueStats } from '../lib/api/issues';
  import { errorMessage } from '../lib/api/client';
  import { relativeTime, formatDateTime, compactNumber } from '../lib/utils/format';
  import type { Issue, IssueStats } from '../lib/models';

  // Issues defaults to "All" time (open issues shouldn't drop off the landing
  // view just because they're old); the picker narrows on demand. 3650d is the
  // backend's effective-all cap for the issues list.
  const ISSUE_RANGES = [
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
    { days: 3650, label: 'All' },
  ];

  // Hydrate filter/search/date-range state from the URL once, at init — not
  // inside an effect, so this never re-runs and never fights the sync effect
  // below.
  const initial = new URLSearchParams($querystring ?? '');
  const parsedFilters = parseFilters(initial.getAll('filter'), ISSUE_FIELDS);
  // Default view: unresolved-only, but ONLY when the URL carried no `filter`
  // at all (so an explicit empty-filter URL, e.g. "show everything", sticks).
  const initialFilters: Filter[] =
    parsedFilters.length === 0 && !initial.has('filter')
      ? [{ field: 'status', op: 'eq', value: 'unresolved' }]
      : parsedFilters;
  let filters = $state<Filter[]>(initialFilters);
  let search = $state(initial.get('q') ?? '');
  // The URL-sync/reload effect below depends on this, not on `search` directly,
  // so free-text typing doesn't fire a backend request + history.replaceState
  // on every keystroke. Filters and the date range still apply immediately.
  let appliedSearch = $state(initial.get('q') ?? '');
  let sinceDays = $state(Number(initial.get('since_days')) || 3650);

  let issues = $state<Issue[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let stats = $state<IssueStats | null>(null);
  let loadingStats = $state(true);

  let refreshing = $state(false);

  const isUnresolvedDefault = $derived(
    !search &&
      filters.length === 1 &&
      filters[0].field === 'status' &&
      filters[0].op === 'eq' &&
      filters[0].value === 'unresolved',
  );

  async function load(appId: string, q: string) {
    loading = true;
    error = null;
    try {
      issues = await listIssues(appId, {
        filters: encodeFilters(filters),
        q: q || undefined,
        sinceDays,
        limit: 100,
      });
    } catch (err) {
      error = errorMessage(err);
      issues = [];
    } finally {
      loading = false;
    }
  }

  async function loadStats(appId: string, days: number) {
    loadingStats = true;
    try {
      stats = await getIssueStats(appId, days);
    } catch {
      stats = null;
    } finally {
      loadingStats = false;
    }
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([load(aid, appliedSearch), loadStats(aid, sinceDays)]);
    } finally {
      refreshing = false;
    }
  }

  // Debounce free-text search only: filters + date range should reload
  // immediately, but typing in the search box should settle before we requery.
  let searchTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const s = search;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      appliedSearch = s;
    }, 300);
    return () => clearTimeout(searchTimer);
  });

  // Re-query + rewrite the URL whenever filter/appliedSearch/date-range state
  // changes. Depends on `appliedSearch` (debounced), not `search`, so this
  // doesn't fire per keystroke.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    const enc = encodeFilters(filters);
    const s = appliedSearch;
    const days = sinceDays;
    if (!aid) return;
    const p = new URLSearchParams();
    for (const f of enc) p.append('filter', f);
    if (s) p.set('q', s);
    p.set('since_days', String(days));
    void replace(`/issues?${p.toString()}`);
    void load(aid, s);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) {
      void loadStats(aid, days);
    }
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Exceptions</h1>
      <p class="muted sub">Grouped errors across your app, most recent first.</p>
    </div>
    <div class="controls">
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  {#if stats}
    <StatTiles min={140}>
      <StatTile label="Total" value={compactNumber(stats.total)} />
      <StatTile label="Unresolved" value={compactNumber(stats.unresolved)} tone="warning" />
      <StatTile label="Resolved" value={compactNumber(stats.resolved)} tone="success" />
      <StatTile label="Ignored" value={compactNumber(stats.ignored)} tone="neutral" />
      <StatTile label="Fatal" value={compactNumber(stats.fatal)} tone="error" />
      <StatTile label="Error" value={compactNumber(stats.error)} tone="error" />
      <StatTile label="Warning" value={compactNumber(stats.warning)} tone="warning" />
    </StatTiles>

    <div class="occ">
      <Card title="Occurrences">
        <TimeSeriesChart data={stats.series} height={200} color="var(--error)" />
      </Card>
    </div>
  {:else if loadingStats}
    <div class="center-sm"><Spinner size={22} /></div>
  {/if}

  <p class="filter-hint">Filter by <code>Tag</code> (key = value); the search box also matches tag &amp; payload content.</p>
  <FilterBar fields={ISSUE_FIELDS} bind:filters bind:search bind:sinceDays ranges={ISSUE_RANGES} />

  <Card padding="none">
    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if error}
      <EmptyState title="Couldn't load issues" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() =>
              sessionStore.currentAppId && load(sessionStore.currentAppId, appliedSearch)}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    {:else if issues.length === 0}
      <EmptyState
        title="No issues here"
        description={isUnresolvedDefault
          ? 'Nothing unresolved right now. Your app is behaving.'
          : 'No issues match these filters.'}
        icon="check"
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th class="col-title">Issue</th>
            <th>Level</th>
            <th>Status</th>
            <th class="num">Events</th>
            <th class="num">Users</th>
            <th>Last seen</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each issues as issue (issue.id)}
            <tr class="clickable" onclick={() => push(`/issues/${issue.id}`)}>
              <td class="col-title">
                <div class="title-cell">
                  <span class="issue-title">{issue.title}</span>
                  <span class="issue-sub mono">
                    {issue.type}{issue.culprit ? ` · ${issue.culprit}` : ''}
                  </span>
                </div>
              </td>
              <td><LevelBadge level={issue.level} size="sm" /></td>
              <td><StatusBadge status={issue.status} size="sm" /></td>
              <td class="num">{issue.times_seen.toLocaleString()}</td>
              <td class="num">{issue.users_seen.toLocaleString()}</td>
              <td class="muted" title={formatDateTime(issue.last_seen)}>
                {relativeTime(issue.last_seen)}
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    {/if}
  </Card>
</AppShell>

<style>
  .filter-hint {
    font-size: 12px;
    color: var(--text-muted);
    margin: -4px 0 8px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .occ {
    margin: 14px 0 18px;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 60px;
  }
  .center-sm {
    display: grid;
    place-items: center;
    padding: 32px;
    margin-bottom: 18px;
  }
  .col-title {
    min-width: 280px;
  }
  .title-cell {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    max-width: 480px;
  }
  .issue-title {
    font-weight: 560;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .issue-sub {
    font-size: 11.5px;
    color: var(--text-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
