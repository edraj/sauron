<script lang="ts">
  import Card from '../src/lib/components/ui/Card.svelte';
  import Pagination from '../src/lib/components/Pagination.svelte';
  import CursorPagination from '../src/lib/components/CursorPagination.svelte';
  import ClientPager from '../src/lib/components/ClientPager.svelte';
  import { pageWindow } from '../src/lib/models/page-window';

  const LIMIT = 50;

  // Offset pager, 10,000 rows = 200 pages. Driven live so the strip can be
  // clicked through and watched for width changes.
  let offset = $state(0);

  // Cursor pager. `page` is authoritative here the way `CursorPage.page` is.
  let cursorPage = $state(2);
  let busy = $state(false);

  // In-memory list — the `ClientPager` case, where the total was never in doubt.
  let clientOffset = $state(0);
  const CLIENT_ROWS = 137;

  /** Every page of a 200-page list, to eyeball the width invariant at once. */
  const widths = $derived(
    [1, 2, 3, 4, 5, 6, 100, 195, 196, 197, 198, 199, 200].map((p) => ({
      p,
      slots: pageWindow(p, 200),
    })),
  );
</script>

<main>
  <h1>Pager render harness</h1>

  <section>
    <h2>Offset pager · 10,000 rows · 200 pages</h2>
    <Card padding="none" title="Screens">
      {#snippet children()}
        <div class="fake-table">
          <p class="muted">table rows would be here — checking the bar below it</p>
        </div>
      {/snippet}
      {#snippet footer()}
        <Pagination
          {offset}
          limit={LIMIT}
          count={LIMIT}
          hasNext={offset + LIMIT < 10_000}
          total={10_000}
          onchange={(o) => (offset = o)}
        />
      {/snippet}
    </Card>
  </section>

  <section>
    <h2>Cursor pager · capped total · busy toggle</h2>
    <label><input type="checkbox" bind:checked={busy} /> busy (load in flight)</label>
    <Card padding="none" title="Transactions">
      {#snippet children()}
        <div class="fake-table">
          <p class="muted">ActiveMobileSubscription · screen load · 197 ms · ok</p>
        </div>
      {/snippet}
      {#snippet footer()}
        <CursorPagination
          total={busy ? null : 10_000}
          totalIsCapped={true}
          page={cursorPage}
          limit={LIMIT}
          canNext={true}
          {busy}
          noun="transaction"
          onjump={(p) => (cursorPage = p)}
        />
      {/snippet}
    </Card>
  </section>

  <section>
    <h2>In-memory pager · 137 rows · 3 pages (no gaps)</h2>
    <Card padding="none" title="Alert channels">
      {#snippet children()}
        <div class="fake-table"><p class="muted">short list — every page listed</p></div>
      {/snippet}
      {#snippet footer()}
        <ClientPager
          offset={clientOffset}
          limit={LIMIT}
          total={CLIENT_ROWS}
          onchange={(o) => (clientOffset = o)}
        />
      {/snippet}
    </Card>
  </section>

  <section>
    <h2>Single page · strip suppressed</h2>
    <Card padding="none" title="Workflows">
      {#snippet children()}
        <div class="fake-table"><p class="muted">12 rows, one page</p></div>
      {/snippet}
      {#snippet footer()}
        <Pagination offset={0} limit={LIMIT} count={12} hasNext={false} total={12} onchange={() => {}} />
      {/snippet}
    </Card>
  </section>

  <section>
    <h2>No results</h2>
    <Card padding="none" title="Devices">
      {#snippet children()}
        <div class="fake-table"><p class="muted">nothing matched</p></div>
      {/snippet}
      {#snippet footer()}
        <Pagination offset={0} limit={LIMIT} count={0} hasNext={false} total={0} onchange={() => {}} />
      {/snippet}
    </Card>
  </section>

  <section>
    <h2>Width invariant · every shape of a 200-page list</h2>
    <table class="slots">
      <thead><tr><th>page</th><th>slots</th><th>n</th></tr></thead>
      <tbody>
        {#each widths as w (w.p)}
          <tr>
            <td class="num">{w.p}</td>
            <td class="mono">{w.slots.map((s) => (s === 'gap' ? '…' : s)).join('  ')}</td>
            <td class="num" class:bad={w.slots.length !== 7}>{w.slots.length}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
</main>

<style>
  main {
    max-width: 1100px;
    margin: 0 auto;
    padding: 32px 24px 80px;
    display: flex;
    flex-direction: column;
    gap: 32px;
  }
  h1 {
    font-size: 20px;
    font-weight: 650;
  }
  h2 {
    font-size: 13px;
    font-weight: 600;
    color: var(--text-muted);
    margin-bottom: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  label {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    font-size: 12.5px;
    color: var(--text-muted);
    margin-bottom: 10px;
  }
  .fake-table {
    padding: 18px;
    border-bottom: 1px solid var(--border);
  }
  .slots {
    width: 100%;
    border-collapse: collapse;
    font-size: 12.5px;
  }
  .slots th,
  .slots td {
    text-align: left;
    padding: 5px 10px;
    border-bottom: 1px solid var(--border);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .mono {
    font-family: var(--font-mono, monospace);
    white-space: pre;
  }
  .bad {
    color: var(--error, red);
    font-weight: 700;
  }
</style>
