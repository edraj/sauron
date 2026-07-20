<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listScreens } from '../lib/api/screens';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber, formatDuration } from '../lib/utils/format';
  import type { ScreenRow } from '../lib/models';

  const LIMIT = 50;

  let sinceDays = $state(30);
  // `query` is bound to the input; `search` is the debounced value that drives loads.
  let query = $state('');
  let search = $state('');
  let offset = $state(0);

  let rows = $state<ScreenRow[]>([]);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);

  let debounce: ReturnType<typeof setTimeout> | undefined;

  function onSearch(v: string) {
    clearTimeout(debounce);
    debounce = setTimeout(() => {
      search = v.trim();
      offset = 0;
    }, 220);
  }

  function onRange(days: number) {
    sinceDays = days;
    offset = 0;
  }

  async function load(appId: string, days: number, s: string, off: number) {
    loading = true;
    error = null;
    try {
      rows = await listScreens(appId, {
        q: s || undefined,
        sinceDays: days,
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
      await Promise.all([load(aid, sinceDays, search, offset)]);
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    const s = search;
    const off = offset;
    if (aid) void load(aid, days, s, off);
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Screens</h1>
      <p class="muted sub">Views, engagement and errors per screen.</p>
    </div>
    <div class="controls">
      <DateRange value={sinceDays} onchange={onRange} />
      <SearchInput bind:value={query} oninput={onSearch} placeholder="Search screens…" width="240px" />
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  {#if error && rows.length === 0}
    <Card>
      <EmptyState title="Couldn't load screens" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, sinceDays, search, offset);
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
        title="No screens yet"
        description={search
          ? `No screens match “${search}”.`
          : 'Call setScreen() in your SDK to attribute events to screens.'}
        icon="layout-panel-top"
      />
    </Card>
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <th>Screen</th>
          <th class="num">Views</th>
          <th class="num">Events</th>
          <th class="num">Exceptions</th>
          <th class="num">Users</th>
          <th class="num">Avg dwell</th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each rows as r (r.screen)}
          <tr class="clickable" onclick={() => push('/screens/' + encodeURIComponent(r.screen))}>
            <td><span class="cell-mono truncate">{r.screen}</span></td>
            <td class="num">{compactNumber(r.views)}</td>
            <td class="num">{compactNumber(r.events)}</td>
            <td class="num"><span class:err={r.exceptions > 0}>{compactNumber(r.exceptions)}</span></td>
            <td class="num">{compactNumber(r.users)}</td>
            <td class="num">{formatDuration(r.avg_dwell_ms)}</td>
          </tr>
        {/each}
      {/snippet}
    </DataTable>

    <Pagination {offset} limit={LIMIT} count={rows.length} onchange={(o) => (offset = o)} />
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
