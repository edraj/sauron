<script lang="ts">
  import { push } from 'svelte-spa-router';
  import DataTable from '../DataTable.svelte';
  import TimeValue from '../TimeValue.svelte';
  import type { DeviceRow } from '../../models';

  interface Props {
    rows: DeviceRow[];
  }
  let { rows }: Props = $props();

  function deviceName(d: DeviceRow): string {
    return [d.family, d.model].filter(Boolean).join(' ').trim();
  }

  function osLabel(d: DeviceRow): string {
    return [d.os_name, d.os_version].filter(Boolean).join(' ').trim() || '—';
  }
</script>

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
