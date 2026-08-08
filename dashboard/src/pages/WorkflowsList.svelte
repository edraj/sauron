<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listWorkflows } from '../lib/api/workflows';
  import { errorMessage } from '../lib/api/client';
  import { completionRate, formatDuration } from '../lib/workflows';
  import { compactNumber, formatPercent } from '../lib/utils/format';
  import type { WorkflowRow } from '../lib/models';

  const LIMIT = 50;

  let sinceDays = $state(30);
  // `search` is bound to the input; `appliedSearch` is the debounced value
  // that drives loads (same split as Issues.svelte).
  let search = $state('');
  let appliedSearch = $state('');
  let offset = $state(0);

  let rows = $state<WorkflowRow[]>([]);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);

  const totals = $derived(
    rows.reduce(
      (acc, r) => ({
        started: acc.started + r.started,
        completed: acc.completed + r.completed,
        abandoned: acc.abandoned + r.abandoned,
      }),
      { started: 0, completed: 0, abandoned: 0 },
    ),
  );
  const totalCompletionRate = $derived(
    totals.started === 0 ? 0 : totals.completed / totals.started,
  );

  function onRange(days: number) {
    sinceDays = days;
    offset = 0;
  }

  function onSearch() {
    offset = 0;
  }

  async function load(appId: string, days: number, s: string, off: number) {
    loading = true;
    error = null;
    try {
      rows = await listWorkflows(appId, {
        since_days: days,
        search: s || undefined,
        limit: LIMIT,
        offset: off,
      });
    } catch (err) {
      error = errorMessage(err);
      rows = [];
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await load(aid, sinceDays, appliedSearch, offset);
    } finally {
      refreshing = false;
    }
  }

  // Debounce free-text search only: typing in the search box should settle
  // before we requery; the date range applies immediately.
  let searchTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const s = search;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      appliedSearch = s;
      offset = 0;
    }, 300);
    return () => clearTimeout(searchTimer);
  });

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    const s = appliedSearch;
    const off = offset;
    if (aid) void load(aid, days, s, off);
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Workflows</h1>
      <p class="muted sub">Named, bounded spans of activity your app reports.</p>
    </div>
    <div class="controls">
      <DateRange value={sinceDays} onchange={onRange} />
      <SearchInput bind:value={search} oninput={onSearch} placeholder="Search workflows…" width="240px" />
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  {#if error && rows.length === 0}
    <Card>
      <EmptyState title="Couldn't load workflows" description={error} icon="workflow">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, sinceDays, appliedSearch, offset);
            }}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if loading && rows.length === 0}
    <div class="center"><Spinner size={26} /></div>
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title="No workflows yet"
        description={appliedSearch
          ? `No workflows match “${appliedSearch}”.`
          : 'Call startWorkflow() in your app to group events into named flows.'}
        icon="workflow"
      />
    </Card>
  {:else}
    <StatTiles min={140}>
      <StatTile label="Started" value={compactNumber(totals.started)} />
      <StatTile label="Completed" value={compactNumber(totals.completed)} tone="success" />
      <StatTile label="Completion rate" value={formatPercent(totalCompletionRate)} />
      <StatTile
        label="Abandoned"
        value={compactNumber(totals.abandoned)}
        tone={totals.abandoned > 0 ? 'error' : 'neutral'}
      />
    </StatTiles>

    <Card padding="none">
      <DataTable>
        {#snippet head()}
          <tr>
            <th>Workflow</th>
            <th class="num">Started</th>
            <th class="num">Completed</th>
            <th class="num">Cancelled</th>
            <th class="num">Abandoned</th>
            <th class="num">Completion rate</th>
            <th class="num">Median</th>
            <th class="num">p95</th>
            <th class="num">Users</th>
            <th class="num">Last seen</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as r (r.name)}
            <tr class="clickable" onclick={() => push('/workflows/' + encodeURIComponent(r.name))}>
              <td><span class="cell-mono truncate">{r.name}</span></td>
              <td class="num">{compactNumber(r.started)}</td>
              <td class="num">{compactNumber(r.completed)}</td>
              <td class="num">{compactNumber(r.cancelled)}</td>
              <td class="num"><span class:err={r.abandoned > 0}>{compactNumber(r.abandoned)}</span></td>
              <td class="num">{formatPercent(completionRate(r))}</td>
              <td class="num">{formatDuration(r.median_duration_ms)}</td>
              <td class="num">{formatDuration(r.p95_duration_ms)}</td>
              <td class="num">{compactNumber(r.unique_users)}</td>
              <td class="num"><TimeValue value={r.last_seen} /></td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>

      <Pagination {offset} limit={LIMIT} count={rows.length} onchange={(o) => (offset = o)} />
    </Card>
  {/if}
</AppShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
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
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .num {
    text-align: right;
  }
  .truncate {
    display: inline-block;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
</style>
