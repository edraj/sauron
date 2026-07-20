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
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import UserActivityChart from '../lib/components/UserActivityChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listPersons } from '../lib/api/persons';
  import { getUserAnalytics } from '../lib/api/users';
  import { errorMessage } from '../lib/api/client';
  import {
    relativeTime,
    formatDateTime,
    initials,
    hueFromString,
    compactNumber,
    formatDuration,
    formatPercent,
  } from '../lib/utils/format';
  import type { PersonRow, UsersAnalytics } from '../lib/models';

  const LIMIT = 50;

  let searchTerm = $state('');
  let query = $state('');
  let offset = $state(0);

  let rows = $state<PersonRow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let debounce: ReturnType<typeof setTimeout> | undefined;

  let sinceDays = $state(30);
  let analytics = $state<UsersAnalytics | null>(null);
  let analyticsError = $state<string | null>(null);

  let refreshing = $state(false);

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([load(aid, query, offset), loadAnalytics(aid, sinceDays)]);
    } finally {
      refreshing = false;
    }
  }

  async function loadAnalytics(appId: string, days: number) {
    analyticsError = null;
    try {
      analytics = await getUserAnalytics(appId, days);
    } catch (err) {
      analyticsError = errorMessage(err);
      analytics = null;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void loadAnalytics(aid, days);
  });

  function onSearch(v: string) {
    clearTimeout(debounce);
    debounce = setTimeout(() => {
      query = v.trim();
      offset = 0;
    }, 250);
  }

  async function load(appId: string, q: string, off: number) {
    loading = true;
    error = null;
    try {
      rows = await listPersons(appId, {
        search: q || undefined,
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

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const q = query;
    const off = offset;
    if (aid) void load(aid, q, off);
  });

  // A compact digest of the most useful person traits, shown in the table.
  const TRAIT_KEYS = ['email', 'plan', 'name'];

  function traits(props: Record<string, unknown> | null): { key: string; value: string }[] {
    if (!props) return [];
    const out: { key: string; value: string }[] = [];
    for (const key of TRAIT_KEYS) {
      const v = props[key];
      if (v !== undefined && v !== null && v !== '') {
        out.push({ key, value: typeof v === 'object' ? JSON.stringify(v) : String(v) });
      }
    }
    return out;
  }

  function open(distinctId: string) {
    push('/persons/' + encodeURIComponent(distinctId));
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Users</h1>
      <p class="muted sub">Identified &amp; anonymous people seen by this app — search by distinct ID or trait.</p>
    </div>
    <div class="controls">
      <SearchInput bind:value={searchTerm} oninput={onSearch} placeholder="Search users…" width="300px" />
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  <div class="analytics-head">
    <h2 class="section-title">Audience</h2>
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
  </div>

  {#if analytics}
    <StatTiles min={150}>
      <StatTile label="Total users" value={compactNumber(analytics.stats.total_users)} tone="primary" sub="all time" />
      <StatTile label="Active" value={compactNumber(analytics.stats.active_in_range)} sub={`last ${sinceDays}d`} />
      <StatTile label="New" value={compactNumber(analytics.stats.new_in_range)} sub={`last ${sinceDays}d`} />
      <StatTile label="WAU" value={compactNumber(analytics.stats.wau)} sub="7-day" />
      <StatTile label="MAU" value={compactNumber(analytics.stats.mau)} sub="30-day" />
      <StatTile label="Stickiness" value={formatPercent(analytics.stickiness)} sub="DAU / MAU" />
      <StatTile label="Avg session" value={formatDuration(analytics.stats.avg_session_ms)} />
      <StatTile label="Median session" value={formatDuration(analytics.stats.median_session_ms)} />
    </StatTiles>

    <Card title="Active users per day">
      <UserActivityChart data={analytics.series} />
    </Card>
  {:else if analyticsError}
    <Card><p class="muted">{analyticsError}</p></Card>
  {/if}

  {#if loading && rows.length === 0}
    <div class="center"><Spinner size={24} /></div>
  {:else if error}
    <Card>
      <EmptyState title="Couldn't load users" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, query, offset);
            }}
          >
            Retry
          </Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if rows.length === 0}
    <Card>
      <EmptyState
        title={query ? 'No matching users' : 'No users yet'}
        description={query
          ? `Nothing matched “${query}”. Try a different distinct ID or trait.`
          : 'Users appear once your SDK identifies people or sends events with a distinct ID.'}
        icon="user"
      />
    </Card>
  {:else}
    <div class="table" class:loading>
      <DataTable>
        {#snippet head()}
          <tr>
            <th>User</th>
            <th>Traits</th>
            <th class="num">Sessions</th>
            <th class="num">Events</th>
            <th class="num">Errors</th>
            <th>First seen</th>
            <th>Last seen</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each rows as row (row.distinct_id)}
            <tr class="clickable" onclick={() => open(row.distinct_id)}>
              <td>
                <span class="user">
                  <span
                    class="avatar"
                    style="background: hsl({hueFromString(row.distinct_id)} 50% 45%)"
                  >
                    {initials(row.distinct_id)}
                  </span>
                  <span class="mono uid" title={row.distinct_id}>{row.distinct_id}</span>
                </span>
              </td>
              <td>
                {#if traits(row.properties).length > 0}
                  <span class="traits">
                    {#each traits(row.properties) as t (t.key)}
                      <span class="trait">
                        <span class="tkey">{t.key}</span>
                        <span class="tval mono">{t.value}</span>
                      </span>
                    {/each}
                  </span>
                {:else}
                  <span class="faint">—</span>
                {/if}
              </td>
              <td class="num">{row.sessions_count.toLocaleString()}</td>
              <td class="num">{row.events_count.toLocaleString()}</td>
              <td class="num">
                <span class:err={row.errors_count > 0}>{row.errors_count.toLocaleString()}</span>
              </td>
              <td class="when muted" title={formatDateTime(row.first_seen)}>
                {relativeTime(row.first_seen)}
              </td>
              <td class="when muted" title={formatDateTime(row.last_seen)}>
                {relativeTime(row.last_seen)}
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    </div>

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
  .analytics-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin: 8px 0 12px;
  }
  .section-title {
    font-size: 15px;
    font-weight: 640;
    margin: 0;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .table {
    transition: opacity 0.12s ease;
  }
  .table.loading {
    opacity: 0.55;
  }
  .user {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .avatar {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
    font-size: 10.5px;
    font-weight: 680;
    flex-shrink: 0;
    text-shadow: 0 1px 1px rgba(0, 0, 0, 0.25);
  }
  .uid {
    font-size: 12px;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: inline-block;
    vertical-align: middle;
  }
  .traits {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .trait {
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    padding: 2px 9px;
    max-width: 220px;
  }
  .tkey {
    font-size: 10px;
    font-weight: 640;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .tval {
    font-size: 11.5px;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    color: var(--error);
    font-weight: 620;
  }
  .when {
    font-size: 12.5px;
  }

  @media (max-width: 640px) {
    .uid {
      max-width: 150px;
    }
  }
</style>
