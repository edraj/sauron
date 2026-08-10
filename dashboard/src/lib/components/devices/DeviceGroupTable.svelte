<script lang="ts">
  import { push } from 'svelte-spa-router';
  import DataTable from '../DataTable.svelte';
  import TimeValue from '../TimeValue.svelte';
  import { encodeGroupKey } from '../../models/device-groups';
  import type { DeviceGroupRow } from '../../models';

  interface Props {
    rows: DeviceGroupRow[];
  }
  let { rows }: Props = $props();

  function deviceName(g: DeviceGroupRow): string {
    return [g.family, g.model].filter(Boolean).join(' ').trim();
  }

  function osLabel(g: DeviceGroupRow): string {
    return [g.os_name, g.os_version].filter(Boolean).join(' ').trim() || '—';
  }

  // The four descriptor columns are the identity of a row here, so they are
  // also its {#each} key — there is no id to fall back on. NUL (`\0`) is the
  // separator, because a Postgres text column can never store a NUL byte —
  // unlike a space, which real family/model values routinely contain. A
  // space separator collides: `{family:'Pixel', model:'7 Pro'}` and
  // `{family:'Pixel 7', model:'Pro'}` both joined to "Pixel 7 Pro" (`|` or
  // `-` are no safer — hyphens and pipes show up in real model strings too).
  // The null placeholder is `''`, deliberately DIFFERENT from the `\0`
  // separator: reusing `\0` for both roles would let `{family:null,
  // model:''}` and `{family:'', model:null}` produce the identical key
  // string, and Svelte throws on a duplicate `{#each}` key in dev AND prod,
  // so that collision took the whole page down rather than just mis-rendering.
  function rowKey(g: DeviceGroupRow): string {
    return [g.family, g.model, g.os_name, g.os_version].map((v) => v ?? '').join('\0');
  }

  function openGroup(g: DeviceGroupRow) {
    push('/devices?' + encodeGroupKey({
      family: g.family,
      model: g.model,
      os_name: g.os_name,
      os_version: g.os_version,
    }));
  }
</script>

<DataTable>
  {#snippet head()}
    <tr>
      <th>Device</th>
      <th>OS</th>
      <th class="num">Devices</th>
      <th class="num">Sessions</th>
      <th class="num">Events</th>
      <th class="num">Errors</th>
      <th>Last seen</th>
    </tr>
  {/snippet}
  {#each rows as g (rowKey(g))}
    <tr class="clickable" onclick={() => openGroup(g)}>
      <td>
        {#if deviceName(g)}
          <span class="dev-name">{deviceName(g)}</span>
        {:else}
          <span class="cell-muted">Unknown device</span>
        {/if}
      </td>
      <td class="cell-muted">{osLabel(g)}</td>
      <td class="num">{g.device_count.toLocaleString()}</td>
      <td class="num">{g.sessions_count.toLocaleString()}</td>
      <td class="num">{g.events_count.toLocaleString()}</td>
      <td class="num">
        <span class:err={g.errors_count > 0}>{g.errors_count.toLocaleString()}</span>
      </td>
      <td><TimeValue value={g.last_seen} muted /></td>
    </tr>
  {/each}
</DataTable>

<style>
  .dev-name {
    font-weight: 560;
    color: var(--text);
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
</style>
