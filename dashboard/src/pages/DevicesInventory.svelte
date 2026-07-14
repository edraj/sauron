<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listDevices } from '../lib/api/devices';
  import { errorMessage } from '../lib/api/client';
  import { relativeTime, formatDateTime } from '../lib/utils/format';
  import type { DeviceRow } from '../lib/models';

  const LIMIT = 50;

  let sinceDays = $state(30);
  // `query` is bound to the input; `search` is the debounced value that drives loads.
  let query = $state('');
  let search = $state('');
  let offset = $state(0);

  let devices = $state<DeviceRow[]>([]);
  let loading = $state(true);
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
      devices = await listDevices(appId, {
        since_days: days,
        search: s || undefined,
        limit: LIMIT,
        offset: off,
      });
    } catch (err) {
      error = errorMessage(err);
      devices = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    const s = search;
    const off = offset;
    if (aid) void load(aid, days, s, off);
  });

  function deviceName(d: DeviceRow): string {
    const label = [d.family, d.model].filter(Boolean).join(' ');
    return label.trim();
  }

  function osLabel(d: DeviceRow): string {
    const label = [d.os_name, d.os_version].filter(Boolean).join(' ');
    return label.trim() || '—';
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Devices</h1>
      <p class="muted sub">Fleet-wide hardware, OS and browser breakdown across your users.</p>
    </div>
    <div class="controls">
      <DateRange value={sinceDays} onchange={onRange} />
      <SearchInput
        bind:value={query}
        oninput={onSearch}
        placeholder="Search devices…"
        width="240px"
      />
    </div>
  </div>

  {#if error && devices.length === 0}
    <Card>
      <EmptyState title="Couldn't load devices" description={error} icon="triangle-alert">
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
  {:else if loading && devices.length === 0}
    <div class="center"><Spinner size={26} /></div>
  {:else if devices.length === 0}
    <Card>
      <EmptyState
        title="No devices found"
        description={search
          ? `No devices match “${search}”.`
          : 'No device telemetry has been reported in this window yet.'}
        icon="monitor"
      />
    </Card>
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <th>Device</th>
          <th>OS</th>
          <th>Browser / Arch</th>
          <th>Last user</th>
          <th class="num">Sessions</th>
          <th class="num">Events</th>
          <th class="num">Errors</th>
          <th>Last seen</th>
        </tr>
      {/snippet}
      {#each devices as d (d.device_key)}
        <tr class="clickable" onclick={() => push('/devices/' + encodeURIComponent(d.device_key))}>
          <td>
            {#if deviceName(d)}
              <span class="dev-name">{deviceName(d)}</span>
            {:else}
              <span class="cell-mono truncate key">{d.device_key}</span>
            {/if}
          </td>
          <td class="cell-muted">{osLabel(d)}</td>
          <td class="cell-muted">{d.browser ?? d.arch ?? '—'}</td>
          <td>
            {#if d.last_distinct_id}
              <a
                class="lnk mono truncate"
                href={`#/persons/${encodeURIComponent(d.last_distinct_id)}`}
                onclick={(e) => e.stopPropagation()}
              >
                {d.last_distinct_id}
              </a>
            {:else}
              <span class="cell-muted">—</span>
            {/if}
          </td>
          <td class="num">{d.sessions_count.toLocaleString()}</td>
          <td class="num">{d.events_count.toLocaleString()}</td>
          <td class="num">
            <span class:err={d.errors_count > 0}>{d.errors_count.toLocaleString()}</span>
          </td>
          <td class="cell-muted" title={formatDateTime(d.last_seen)}>
            {relativeTime(d.last_seen)}
          </td>
        </tr>
      {/each}
    </DataTable>

    <Pagination {offset} limit={LIMIT} count={devices.length} onchange={(o) => (offset = o)} />
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
  .dev-name {
    font-weight: 560;
    color: var(--text);
  }
  .key {
    display: inline-block;
    max-width: 220px;
    color: var(--text-muted);
  }
  .lnk {
    display: inline-block;
    max-width: 200px;
    color: var(--text-muted);
    font-size: 12px;
  }
  .lnk:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
</style>
