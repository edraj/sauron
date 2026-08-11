<script lang="ts">
  import { querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import DeviceFlatTable from '../lib/components/devices/DeviceFlatTable.svelte';
  import DeviceGroupTable from '../lib/components/devices/DeviceGroupTable.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listDevices, listDeviceGroups } from '../lib/api/devices';
  import { decodeGroupKey, encodeGroupKey, groupLabel, sameGroupKey } from '../lib/models/device-groups';
  import type { DeviceRow, DeviceGroupRow } from '../lib/models';

  const LIMIT = 50;

  // Hydrate the drill-down key from the URL once, at init — not inside an
  // effect, so it never re-runs and never fights the sync effect below. Same
  // pattern as Issues.svelte:44 and Events.svelte:33.
  let groupKey = $state(decodeGroupKey($querystring ?? null));
  const grouped = $derived(groupKey === null);

  let sinceDays = $state(30);
  // `query` is bound to the input; `search` is the debounced value that drives loads.
  let query = $state('');
  let search = $state('');
  let offset = $state(0);

  // Two cached views, one per mode. Separate instances rather than one shared:
  // the payloads are different types, and keeping them apart means switching
  // modes repaints from cache instead of re-fetching.
  const groupView = new CachedView<DeviceGroupRow[]>();
  const flatView = new CachedView<DeviceRow[]>();

  const groups = $derived(groupView.data ?? []);
  const devices = $derived(flatView.data ?? []);
  const rowCount = $derived(grouped ? groups.length : devices.length);

  // Cached view (lib/stores/cached-view.svelte.ts): rows already fetched paint
  // instantly on return, then refresh behind a spinner instead of a skeleton.
  // Re-exposed under the names the template already used, so the markup is
  // unchanged apart from the refresh indicator.
  const view = $derived(grouped ? groupView : flatView);
  const revalidating = $derived(view.revalidating);
  const loading = $derived(view.loading);
  let refreshing = $state(false);
  const error = $derived(view.error);

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

  // `scopeKey` must be in the key: it carries the selected environment, which
  // the axios interceptor adds to the request but which appears in none of
  // these arguments. Omit it and one environment's rows are served as another's.
  //
  // The group key is in the cache key too, for the same reason — two drill-downs
  // differ only by it.
  //
  // `force` bypasses the fresh-window short-circuit — an explicit Refresh or
  // Retry means "go to the network now".
  async function load(appId: string, days: number, s: string, off: number, force = false) {
    const params = { since_days: days, search: s || undefined, limit: LIMIT, offset: off };
    if (groupKey === null) {
      await groupView.load(
        viewKey('devices.groups', appId, sessionStore.scopeKey, days, s, off, LIMIT),
        () => listDeviceGroups(appId, params),
        force,
      );
      return;
    }
    const k = groupKey;
    await flatView.load(
      viewKey('devices.list', appId, sessionStore.scopeKey, days, s, off, LIMIT, encodeGroupKey(k)),
      () => listDevices(appId, {
        ...params,
        group: '1',
        // `?? undefined`, so a NULL component is omitted from the request and
        // the backend reads it as SQL NULL. Sending `''` would filter to the
        // empty string instead, which is a different group.
        family: k.family ?? undefined,
        model: k.model ?? undefined,
        os_name: k.os_name ?? undefined,
        os_version: k.os_version ?? undefined,
      }),
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    const s = search;
    const off = offset;
    // Touch groupKey so entering or leaving a drill-down refetches.
    groupKey;
    if (aid) void load(aid, days, s, off);
  });

  // The router owns the URL; the page follows it. `push`ing a drill-down URL
  // from the grouped table updates `$querystring`, and this is what turns that
  // into a mode change. Resetting `offset` here rather than at the call site
  // covers the browser Back button too, which no click handler sees.
  $effect(() => {
    const next = decodeGroupKey($querystring ?? null);
    // `sameGroupKey`, not an `encodeGroupKey` string comparison: encoding
    // omits null components, so a "no group" sentinel and the REAL all-NULL
    // group both encode to `"group=1"` and would compare equal, wedging the
    // page in grouped mode forever (and, from a drill-down, wedging the
    // crumb's "All devices" button / browser Back, since neither could ever
    // register the URL change back to `/devices`). `sameGroupKey` checks
    // `null` explicitly before comparing fields, so it can't make that
    // mistake. See device-groups.ts's doc comment and its `sameGroupKey`
    // test cases for the exact collision this replaced.
    if (!sameGroupKey(next, groupKey)) {
      groupKey = next;
      offset = 0;
    }
  });

  function backToGroups() {
    replace('/devices');
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([load(aid, sinceDays, search, offset, true)]);
    } finally {
      refreshing = false;
    }
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
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
    </div>
  </div>

  <!--
    Hoisted above the loading/error/empty/loaded chain below, on purpose: a
    search or date-range narrowing can empty out or error a drill-down, and
    the crumb is the ONLY way back to the grouped view short of the browser's
    own Back button. Keeping it inside the `{:else}` (loaded-with-rows) branch
    would strand the user in exactly that case.

    `groupKey` alongside `!grouped` is not redundant despite `grouped` being
    derived from it (`grouped = groupKey === null`): Svelte's `{#if}` doesn't
    narrow `groupKey`'s type from the truthiness of a separate `grouped`
    variable, so `groupLabel(groupKey)` below needs its own null check to
    type-check. Do not simplify this to `{#if !grouped}`.
  -->
  {#if !grouped && groupKey}
    <div class="crumb">
      <button class="back" onclick={backToGroups} type="button">
        <Icon name="arrow-left" size={14} />
        All devices
      </button>
      <span class="chip">{groupLabel(groupKey)}</span>
    </div>
  {/if}

  {#if error && rowCount === 0}
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
  {:else if loading && rowCount === 0}
    <div class="center"><Spinner size={26} /></div>
  {:else if rowCount === 0}
    <Card>
      <EmptyState
        title={grouped ? 'No devices found' : 'No devices in this group'}
        description={search
          ? `No devices match “${search}”.`
          : grouped
            ? 'No device telemetry has been reported in this window yet.'
            : 'This model and OS has no devices in the selected window.'}
        icon="monitor"
      />
    </Card>
  {:else}
    {#if grouped}
      <DeviceGroupTable rows={groups} />
    {:else}
      <DeviceFlatTable rows={devices} />
    {/if}

    <!-- Slice 3 replaces this with a `limit + 1` over-fetch probe. Until then
         this reproduces the old (wrong) inference rather than hiding it: a final
         page of exactly `limit` rows still offers a Next to an empty page. -->
    <Pagination
      {offset}
      limit={LIMIT}
      count={rowCount}
      hasNext={rowCount >= LIMIT}
      onchange={(o) => (offset = o)}
    />
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
  .crumb {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 12px;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
    padding: 0;
  }
  .back:hover {
    color: var(--text);
  }
  .chip {
    font-size: 12.5px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 3px 8px;
  }
</style>
