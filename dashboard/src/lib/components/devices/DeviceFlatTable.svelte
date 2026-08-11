<script lang="ts">
  import { push } from 'svelte-spa-router';
  import DataTable from '../DataTable.svelte';
  import SortableTh from '../SortableTh.svelte';
  import TimeValue from '../TimeValue.svelte';
  import type { SortDir, SortState } from '../../models/sort';
  import type { DeviceRow } from '../../models';

  interface Props {
    rows: DeviceRow[];
    /**
     * Sort state and the click handler both come from `DevicesInventory`,
     * which owns the one `OffsetListState` these headers drive: the sort has
     * to reset the offset, and only the page holds that.
     */
    sort: SortState;
    onsort: (key: string, columnDefault: SortDir) => void;
  }
  let { rows, sort, onsort }: Props = $props();

  function deviceName(d: DeviceRow): string {
    return [d.family, d.model].filter(Boolean).join(' ').trim();
  }

  function osLabel(d: DeviceRow): string {
    return [d.os_name, d.os_version].filter(Boolean).join(' ').trim() || '—';
  }
</script>

<DataTable>
  {#snippet head()}
    <!--
      DEVICE AND OS ARE PLAIN <th> ON PURPOSE — do not "restore" them.

      This component is only ever rendered inside a group DRILL-DOWN:
      `DevicesInventory` renders it in the `{:else}` of `{#if grouped}`, and
      `grouped` is `groupKey === null`. A non-null group key means the request
      carries `group=1` plus the four descriptor fields, which `list_devices`
      pins with `IS NOT DISTINCT FROM`. So every row this table can render
      shares ONE family, model, os_name and os_version.

      `family` and `os_name` are constant across those rows, and
      `d.family ASC, d.device_key ASC` and `d.family DESC, d.device_key ASC`
      are the same sequence when `d.family` is constant. A `SortableTh` here
      would fire a real request, get a valid 200, flip its caret, and move no
      row — against the design spec's own rule that an unsorted column is
      honest and a wrongly-sorted one is not.

      The backend whitelist still accepts `family`/`os_name` and is correct:
      `/devices` without `group=1` is a legal call where they are meaningful.
      Nothing in the dashboard makes that call. If a general flat device list
      is ever added, make these `SortableTh` again THERE — the sort state
      already resets on drill-down change, so the two cannot leak into each
      other.

      They remain sortable on `DeviceGroupTable`, where the descriptors vary.

      Browser / Arch stays sortable and still renders a PAIR sorted by its
      first half only (`browser ?? arch`, sorts by `browser`), which is the
      whitelist the backend offers and was ruled acceptable for this slice —
      do not add a second sort key to close that gap. Within one `browser` the
      order is the tiebreak's, not `arch`'s.
    -->
    <tr>
      <th>Device</th>
      <th>OS</th>
      <SortableTh key="browser" columnDefault="asc" {sort} {onsort}>Browser / Arch</SortableTh>
      <SortableTh key="distinct_id" columnDefault="asc" {sort} {onsort}>Last user</SortableTh>
      <SortableTh key="sessions_count" class="num" {sort} {onsort}>Sessions</SortableTh>
      <SortableTh key="events_count" class="num" {sort} {onsort}>Events</SortableTh>
      <SortableTh key="errors_count" class="num" {sort} {onsort}>Errors</SortableTh>
      <SortableTh key="last_seen" {sort} {onsort}>Last seen</SortableTh>
    </tr>
  {/snippet}
  {#each rows as d (d.device_key)}
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
      <td><TimeValue value={d.last_seen} muted /></td>
    </tr>
  {/each}
</DataTable>

<style>
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
