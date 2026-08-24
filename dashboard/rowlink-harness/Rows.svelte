<script lang="ts">
  /**
   * Render harness for the clickable-row link work.
   *
   * Everything here that matters is invisible to the static gates. `svelte-check`
   * and the unit suite are both happy with a row whose `onauxclick` Svelte never
   * binds, an `a[href]` guard that misses because the event target is the span
   * INSIDE the anchor, a `window.open` that fires twice, and a first cell whose
   * layout collapsed when its wrapper `<div>` became an `<a>`. Those are all
   * event plumbing and render, so they are checked here instead.
   *
   * The row is built to match the real ones: a first-cell link to the row's own
   * record, a nested link to a DIFFERENT record in column 2 (SessionsList links
   * out to the person and the device), the real TimeValue toggle in column 3,
   * and a plain cell in column 4.
   */
  import DataTable from '../src/lib/components/DataTable.svelte';
  import TimeValue from '../src/lib/components/TimeValue.svelte';
  import { rowHref, rowNav } from '../src/lib/utils/row-link';

  const ROWS = [
    { id: 's1', person: 'p1', at: '2026-08-24T09:00:00Z', events: 41 },
    { id: 's2', person: 'p2', at: '2026-08-24T08:00:00Z', events: 7 },
  ];

  /** Every window.open the page attempts, so a DOUBLE open is visible. */
  let opened = $state<string[]>([]);
  const nativeOpen = window.open.bind(window);
  window.open = ((url?: string | URL) => {
    opened = [...opened, String(url)];
    return null;
  }) as typeof nativeOpen;

  let hash = $state(location.hash);
  addEventListener('hashchange', () => (hash = location.hash));

  function reset() {
    opened = [];
    location.hash = '#/start';
  }
</script>

<h1>row-link harness</h1>

<DataTable>
  {#snippet head()}
    <tr>
      <th>Session</th>
      <th>Person</th>
      <th>Last seen</th>
      <th class="num">Events</th>
    </tr>
  {/snippet}
  {#each ROWS as r (r.id)}
    {@const path = '/sessions/' + r.id}
    <tr class="clickable" onclick={(e) => rowNav(e, path)} onauxclick={(e) => rowNav(e, path)}>
      <td><a class="row-link cell-mono" href={rowHref(path)} data-t="own-{r.id}">{r.id}</a></td>
      <td>
        <a
          class="cell-mono"
          href={`#/persons/${r.person}`}
          onclick={(e) => e.stopPropagation()}
          data-t="other-{r.id}"
        >
          {r.person}
        </a>
      </td>
      <td data-t="time-{r.id}"><TimeValue value={r.at} muted /></td>
      <td class="num" data-t="plain-{r.id}">{r.events}</td>
    </tr>
  {/each}
</DataTable>

<p>
  hash: <code data-t="hash">{hash}</code><br />
  opened ({opened.length}): <code data-t="opened">{JSON.stringify(opened)}</code>
</p>
<button type="button" onclick={reset} data-t="reset">reset</button>

<style>
  h1 {
    font-size: 15px;
    margin: 16px;
  }
  p,
  button {
    margin: 16px;
    font-family: var(--font-mono);
    font-size: 12px;
  }
</style>
