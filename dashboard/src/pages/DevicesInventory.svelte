<script lang="ts">
  import { t } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import DeviceFlatTable from '../lib/components/devices/DeviceFlatTable.svelte';
  import DeviceGroupTable from '../lib/components/devices/DeviceGroupTable.svelte';
  import TimeFilter from '../lib/components/TimeFilter.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { listDevices, listDeviceGroups } from '../lib/api/devices';
  import { countDevices } from '../lib/api/counts';
  import { RowCount } from '../lib/stores/row-count.svelte';
  import { decodeGroupKey, encodeGroupKey, groupLabel, sameGroupKey } from '../lib/models/device-groups';
  import {
    setOffsetPage,
    setOffsetSort,
    type ListPage,
    type OffsetListState,
  } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import {
    fromParams,
    toParams,
    toRecord,
    type TimeField,
    type TimeFilterState,
  } from '../lib/models/time-filter';
  import type { DeviceRow, DeviceGroupRow } from '../lib/models';

  const LIMIT = 50;

  /**
   * The columns this list can be windowed by, both indexed as of migration
   * 000062 (`devices_app_first_seen_idx`, `device_env_app_env_first_seen_idx`).
   *
   * The window decides WHICH DEVICES ARE LISTED, via the durable `devices`
   * column — it is not a predicate on the value each row displays. Under a
   * scoped read the displayed `first_seen`/`last_seen` are per-environment
   * extrema derived from LATERALs, and a device's per-environment first
   * sighting can postdate its app-level one. That is what `since_days` has
   * always meant here, it is the only form an index can serve, and it is the
   * OPPOSITE of what the Users list means by the same words.
   */
  const TIME_FIELDS: TimeField[] = [
    { key: 'last_seen', label: 'Last seen' },
    { key: 'first_seen', label: 'First seen' },
  ];
  const DEFAULT_TIME_FIELD = 'last_seen';
  const DEFAULT_DAYS = 30;

  // Hydrate the drill-down key from the URL once, at init — not inside an
  // effect, so it never re-runs and never fights the sync effect below. Same
  // pattern as Issues.svelte:44 and Events.svelte:33.
  let groupKey = $state(decodeGroupKey($querystring ?? null));
  const grouped = $derived(groupKey === null);

  // Hydrated from the URL once, at init, for the same reason `groupKey` is —
  // not inside an effect, so it never re-runs and never fights the sync effect
  // that writes the URL back.
  let timeFilter = $state<TimeFilterState>(
    fromParams(
      new URLSearchParams($querystring ?? ''),
      TIME_FIELDS,
      DEFAULT_TIME_FIELD,
      DEFAULT_DAYS,
    ),
  );

  // `query` is bound to the input; `search` is the SUBMITTED value that drives loads.
  let query = $state('');
  let search = $state('');

  /**
   * `last_seen` descending is what both endpoints default to, so this state
   * describes the first request rather than changing it.
   *
   * A function rather than a shared module constant because it is assigned
   * from two places (initialisation and the mode-switch reset below), and
   * `list` is `$state` — handing the same object to both would put one
   * long-lived target behind every reset instead of a fresh value each time.
   * Nothing here mutates it, so this is cheap insurance rather than a fix for
   * an observed bug.
   */
  function initialList(): OffsetListState {
    return { sort: { key: 'last_seen', dir: 'desc' }, offset: 0 };
  }

  // ONE state for both tables — only one is ever on screen. See the reset in
  // the group-key sync effect below for why switching between them does not
  // carry the sort across.
  let list = $state<OffsetListState>(initialList());

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  // Two cached views, one per mode. Separate instances rather than one shared:
  // the payloads are different types, and keeping them apart means switching
  // modes repaints from cache instead of re-fetching.
  const groupView = new CachedView<ListPage<DeviceGroupRow>>();
  const flatView = new CachedView<ListPage<DeviceRow>>();

  const groups = $derived(groupView.data?.rows ?? []);
  const devices = $derived(flatView.data?.rows ?? []);
  const rowCount = $derived(grouped ? groups.length : devices.length);
  // Read off the cached payload rather than a separate `$state` set on the
  // network path: a cache HIT repaints rows without fetching, and a `hasNext`
  // that only the fetch updates would be the previous key's answer.
  const hasNext = $derived(
    (grouped ? groupView.data?.hasNext : flatView.data?.hasNext) ?? false,
  );
  /** Total matching rows across all pages — distinct from `rowCount`, which is this page's. */
  const totalRows = new RowCount();

  // Cached view (lib/stores/cached-view.svelte.ts): rows already fetched paint
  // instantly on return, then refresh behind a spinner instead of a skeleton.
  // Re-exposed under the names the template already used, so the markup is
  // unchanged apart from the refresh indicator.
  const view = $derived(grouped ? groupView : flatView);
  const revalidating = $derived(view.revalidating);
  const loading = $derived(view.loading);
  let refreshing = $state(false);
  const error = $derived(view.error);

  function downloadDevicesCsv() {
    let header: string[] = [];
    let rows: any[][] = [];

    if (grouped) {
      if (!groups || groups.length === 0) return;
      header = [
        'Family', 'Model', 'OS Name', 'OS Version', 'Devices',
        'Events', 'Errors', 'Sessions', 'First Seen', 'Last Seen'
      ];
      rows = groups.map((g) => [
        g.family ?? '',
        g.model ?? '',
        g.os_name ?? '',
        g.os_version ?? '',
        g.device_count,
        g.events_count,
        g.errors_count,
        g.sessions_count,
        g.first_seen ? new Date(g.first_seen).toISOString() : '',
        g.last_seen ? new Date(g.last_seen).toISOString() : ''
      ]);
    } else {
      if (!devices || devices.length === 0) return;
      header = [
        'ID', 'Device Key', 'Family', 'Model', 'OS Name', 'OS Version',
        'Arch', 'Browser', 'Last Distinct ID', 'First Seen', 'Last Seen',
        'Events', 'Errors', 'Sessions'
      ];
      rows = devices.map((d) => [
        d.id,
        d.device_key,
        d.family ?? '',
        d.model ?? '',
        d.os_name ?? '',
        d.os_version ?? '',
        d.arch ?? '',
        d.browser ?? '',
        d.last_distinct_id ?? '',
        d.first_seen ? new Date(d.first_seen).toISOString() : '',
        d.last_seen ? new Date(d.last_seen).toISOString() : '',
        d.events_count,
        d.errors_count,
        d.sessions_count
      ]);
    }

    const csvContent = [header, ...rows]
      .map((row) => row.map(v => typeof v === 'string' && v.includes(',') ? `"${v}"` : v).join(','))
      .join('\n');
    const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = grouped ? 'device-groups.csv' : 'devices.csv';
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }

  // Submit-driven, not debounced: `query` is the text in the box and
  // `search` is what the request carries. Only the Search button, Enter and
  // the clear button move one into the other, so typing never queries.
  function onSearch(v: string) {
    search = v.trim();
    // A changed predicate invalidates the page position: row 51 of the old
    // result set is not row 51 of the new one.
    list = setOffsetPage(list, 0);
  }

  function onTimeFilter(v: TimeFilterState) {
    timeFilter = v;
    // A changed predicate invalidates the page position: row 51 of the old
    // window is not row 51 of the new one.
    list = setOffsetPage(list, 0);
  }

  /**
   * Keep the window in the URL, alongside whatever drill-down is active.
   *
   * Composed with `encodeGroupKey` rather than written on its own, because the
   * group and the window share one query string and two writers that each
   * assume they own it would take turns deleting the other's parameters.
   *
   * This also SELF-HEALS the drill-down navigation: `DeviceGroupTable` pushes
   * `/devices?` + `encodeGroupKey(...)`, which carries no window at all, so
   * without this effect entering a group would silently drop the filter from
   * the URL and a refresh would come back on the default window.
   *
   * It cannot loop with the `groupKey` sync effect below: that one re-decodes
   * the group from the string this writes, finds it unchanged, and is stopped
   * by its own `sameGroupKey` guard before it assigns anything.
   */
  $effect(() => {
    const p = toParams(timeFilter, DEFAULT_TIME_FIELD);
    const g = groupKey === null ? '' : encodeGroupKey(groupKey);
    const qs = [g, p.toString()].filter(Boolean).join('&');
    void replace(qs ? `/devices?${qs}` : '/devices');
  });

  // `scopeKey` must be in the key: it carries the selected environment, which
  // the axios interceptor adds to the request but which appears in none of
  // these arguments. Omit it and one environment's rows are served as another's.
  //
  // The group key is in the cache key too, for the same reason — two drill-downs
  // differ only by it.
  //
  // `sort` is in the cache key for the same reason every other varying input
  // is: without it a header click finds the previous ordering already cached
  // under the same key and repaints it with NO request on the wire — the sort
  // looks like it silently did nothing.
  //
  // `force` bypasses the fresh-window short-circuit — an explicit Refresh or
  // Retry means "go to the network now".
  async function load(
    appId: string,
    tf: TimeFilterState,
    s: string,
    sort: string,
    off: number,
    force = false,
  ) {
    const params = {
      search: s || undefined,
      sort,
      limit: LIMIT,
      offset: off,
      ...toRecord(tf, DEFAULT_TIME_FIELD),
    };
    // The window enters the key as its DECLARATION, never as the instant `last`
    // resolves to: a clock-derived component mints a fresh entry on every load,
    // so the cache stays wired and typed while hitting zero times — invisible
    // from the DOM, visible only in the network panel.
    const windowKey = `${tf.field}:${tf.mode}:${tf.lastDays ?? ''}:${tf.from ?? ''}:${tf.to ?? ''}`;
    // Predicate only, but `grouped` IS part of it: the two shapes count
    // different things (descriptor tuples vs individual devices) and their
    // totals differ by a large factor, so a key that ignored it would caption
    // one table with the other's number and look merely surprising.
    void totalRows.load(
      viewKey('devices.count', appId, sessionStore.scopeKey, windowKey, s, String(grouped)),
      () =>
        countDevices(appId, {
          grouped,
          search: s || undefined,
          window: toRecord(tf, DEFAULT_TIME_FIELD),
          family: groupKey?.family ?? undefined,
          model: groupKey?.model ?? undefined,
          osName: groupKey?.os_name ?? undefined,
          osVersion: groupKey?.os_version ?? undefined,
        }),
      force,
    );
    if (groupKey === null) {
      await groupView.load(
        viewKey('devices.groups', appId, sessionStore.scopeKey, windowKey, s, sort, off, LIMIT),
        () => listDeviceGroups(appId, params),
        force,
      );
      return;
    }
    const k = groupKey;
    await flatView.load(
      viewKey(
        'devices.list',
        appId,
        sessionStore.scopeKey,
        windowKey,
        s,
        sort,
        off,
        LIMIT,
        encodeGroupKey(k),
      ),
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
    const tf = timeFilter;
    const s = search;
    const sort = sortParam(list.sort);
    const off = list.offset;
    // Touch groupKey so entering or leaving a drill-down refetches.
    groupKey;
    if (aid) void load(aid, tf, s, sort, off);
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
      // The SORT resets here too, not only the offset. The two device
      // endpoints do not share a sort whitelist — `browser` and `distinct_id`
      // exist only on the flat list, `device_count` only on the grouped one —
      // and an unlisted column is a 400, not a silently ignored parameter. So
      // drilling into a group while sorted by Devices, or leaving one while
      // sorted by Browser, would fail the very request the click was for.
      list = initialList();
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
      await Promise.all([
        load(aid, timeFilter, search, sortParam(list.sort), list.offset, true),
      ]);
    } finally {
      refreshing = false;
    }
  }

</script>

  <div class="head">
    <div>
      <h1 class="page-title">{t('devices.title')}</h1>
      <p class="muted sub">{t('devices.subtitle')}</p>
    </div>
    <!-- Search first, then the window, then refresh — the same order Sessions
         and Users put these controls in. This page keeps them in the header
         because its table starts directly below: there is no analytics section
         in between for the toolbar to drift away from. -->
    <div class="controls">
      <SearchInput
        bind:value={query}
        onsearch={onSearch}
        placeholder={t('devices.search')}
        width="240px"
      />
      <TimeFilter fields={TIME_FIELDS} value={timeFilter} onchange={onTimeFilter} />
      <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
      <Button
        variant="secondary"
        disabled={rowCount === 0}
        onclick={downloadDevicesCsv}
        title={t('devices.exportTitle')}
      >
        <Icon name="download" size={15} />
        {t('explore.exportCsv')}
      </Button>
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
        {t('devices.all')}
      </button>
      <span class="chip">{groupLabel(groupKey)}</span>
    </div>
  {/if}

  {#if error && rowCount === 0}
    <Card>
      <EmptyState title={t('devices.error.load')} description={error} icon="triangle-alert">
        {#snippet action()}
          <Button
            variant="secondary"
            onclick={() => {
              const aid = sessionStore.currentAppId;
              if (aid) load(aid, timeFilter, search, sortParam(list.sort), list.offset);
            }}
          >
            {t('common.retry')}
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
      <DeviceGroupTable rows={groups} sort={list.sort} {onsort} />
    {:else}
      <DeviceFlatTable rows={devices} sort={list.sort} {onsort} />
    {/if}

    <!-- `hasNext` is the client's `limit + 1` over-fetch probe, not an
         inference from the row count: a final page of exactly `LIMIT` rows
         used to offer a Next that led to an empty page. -->
    <Pagination
      offset={list.offset}
      limit={LIMIT}
      count={rowCount}
      {hasNext}
      total={totalRows.total}
      totalIsCapped={totalRows.isCapped}
      onchange={(o) => (list = setOffsetPage(list, o))}
    />
  {/if}

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
