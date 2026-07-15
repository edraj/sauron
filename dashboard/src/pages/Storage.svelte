<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import { getAdminStorage } from '../lib/api/admin';
  import type { StorageReport } from '../lib/api/admin';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';

  let report = $state<StorageReport | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Which app rows are expanded to show their cold Parquet file inventory.
  let openApp = $state<Record<string, boolean>>({});
  function toggleApp(appId: string) {
    openApp = { ...openApp, [appId]: !openApp[appId] };
  }

  async function load() {
    loading = true;
    error = null;
    try {
      report = await getAdminStorage();
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const u = ['KB', 'MB', 'GB', 'TB'];
    let v = n / 1024, i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(1)} ${u[i]}`;
  }

  $effect(() => { void load(); });
</script>

<AppShell requireProject={false}>
  <div class="storage">
    <header class="head">
      <div>
        <h1 class="page-title">Storage</h1>
        <p class="sub muted">Deployment-wide database size and per-app hot/cold record storage.</p>
      </div>
    </header>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if report}
      {@const rep = report}
      <StatTiles min={180}>
        <StatTile label="Database size" value={fmtBytes(rep.database.total_bytes)} tone="primary" />
        <StatTile label="Tables" value={rep.database.tables.length} />
        <StatTile label="Apps" value={rep.apps.length} />
      </StatTiles>

      <div class="section">
        <Card title="Database tables" padding="none">
          {#if rep.database.tables.length === 0}
            <EmptyState title="No tables" description="No tiered tables were reported." icon="server" />
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <th>Table</th>
                  <th class="num">Size</th>
                  <th class="num">Hot rows</th>
                </tr>
              {/snippet}
              {#snippet children()}
                {#each rep.database.tables as t (t.name)}
                  <tr>
                    <td><span class="cell-mono">{t.name}</span></td>
                    <td class="num">{fmtBytes(t.total_bytes)}</td>
                    <td class="num">{t.hot_rows.toLocaleString()}</td>
                  </tr>
                {/each}
              {/snippet}
            </DataTable>
          {/if}
        </Card>
      </div>

      <div class="section">
        <Card title="Storage by app" padding="none">
          {#if rep.apps.length === 0}
            <EmptyState title="No apps" description="No apps have been created yet." icon="package" />
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <th>App</th>
                  <th>Org</th>
                  <th class="num">Hot rows</th>
                  <th class="num">Cold rows</th>
                  <th class="num">Cold bytes</th>
                  <th class="num">Est. hot bytes</th>
                </tr>
              {/snippet}
              {#snippet children()}
                {#each rep.apps as a (a.app_id)}
                  <tr class="clickable" onclick={() => toggleApp(a.app_id)}>
                    <td>
                      <div class="name-cell">
                        <span class="chevron" class:open={openApp[a.app_id]}>
                          <Icon name="chevron-right" size={14} />
                        </span>
                        <span class="name">{a.app_name}</span>
                      </div>
                    </td>
                    <td><span class="cell-muted">{a.org_name}</span></td>
                    <td class="num">{a.hot_rows_total.toLocaleString()}</td>
                    <td class="num">{a.cold_rows_total.toLocaleString()}</td>
                    <td class="num">{fmtBytes(a.cold_bytes_total)}</td>
                    <td class="num">{fmtBytes(a.estimated_hot_bytes_total)}</td>
                  </tr>
                  {#if openApp[a.app_id]}
                    <tr class="expand-row">
                      <td colspan="6" style="background: var(--surface-2); white-space: normal; cursor: default;">
                        <div class="expand-body">
                          <h4 class="expand-title">Per-table breakdown</h4>
                          <!--
                            A CSS grid, not a nested <table> — a raw <table> here would sit
                            inside DataTable's own <tbody>/<td> and pick up its scoped-but-
                            :global() `tbody td` / `td.num` rules (padding, white-space,
                            alignment) by DOM descendance, regardless of component
                            boundaries. See the `uptimeColor` inline-style note in
                            Monitors.svelte for the same trap on a different property.
                          -->
                          <div class="mini-grid" role="table" aria-label="Per-table breakdown">
                            <div class="mini-row mini-head" role="row">
                              <span role="columnheader">Table</span>
                              <span class="num" role="columnheader">Hot rows</span>
                              <span class="num" role="columnheader">Cold rows</span>
                              <span class="num" role="columnheader">Cold bytes</span>
                              <span class="num" role="columnheader">Est. hot bytes</span>
                            </div>
                            {#each a.tables as t (t.name)}
                              <div class="mini-row" role="row">
                                <span class="cell-mono" role="cell">{t.name}</span>
                                <span class="num" role="cell">{t.hot_rows.toLocaleString()}</span>
                                <span class="num" role="cell">{t.cold_rows.toLocaleString()}</span>
                                <span class="num" role="cell">{fmtBytes(t.cold_bytes)}</span>
                                <span class="num" role="cell">{fmtBytes(t.estimated_hot_bytes)}</span>
                              </div>
                            {/each}
                          </div>

                          <h4 class="expand-title">Cold Parquet files ({a.cold_files.length})</h4>
                          {#if a.cold_files.length === 0}
                            <p class="faint">No cold files for this app.</p>
                          {:else}
                            <ul class="file-list">
                              {#each a.cold_files as f (f.path)}
                                <li>
                                  <span class="cell-mono file-path" title={f.path}>{f.path}</span>
                                  <span class="cell-muted file-size">{fmtBytes(f.bytes)}</span>
                                </li>
                              {/each}
                            </ul>
                          {/if}
                        </div>
                      </td>
                    </tr>
                  {/if}
                {/each}
              {/snippet}
            </DataTable>
          {/if}
        </Card>
      </div>
    {/if}
  </div>
</AppShell>

<style>
  .storage {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  /* --- header --------------------------------------------------------------- */
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }

  /* --- error banner --------------------------------------------------------- */
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    font-size: 13px;
    color: var(--error);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 38%, transparent);
    border-radius: var(--radius);
  }

  .center {
    display: grid;
    place-items: center;
    min-height: 180px;
  }

  .section {
    display: flex;
    flex-direction: column;
  }

  /* --- app row / expander ---------------------------------------------------- */
  .name-cell {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .name {
    font-weight: 560;
  }
  .chevron {
    display: inline-flex;
    color: var(--text-faint);
    transition: transform 0.14s ease;
  }
  .chevron.open {
    transform: rotate(90deg);
  }

  /* The expand-row <td>'s background/white-space/cursor are set inline (see
     markup) rather than here — DataTable's own scoped-but-:global() `tbody td`
     rule would otherwise win the specificity fight. */
  .expand-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 6px 4px 10px;
  }
  .expand-title {
    font-size: 11px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    margin-top: 6px;
  }
  .expand-title:first-child {
    margin-top: 0;
  }

  .mini-grid {
    display: flex;
    flex-direction: column;
    font-size: 12.5px;
  }
  .mini-row {
    display: grid;
    grid-template-columns: 1.6fr repeat(4, 1fr);
    gap: 8px;
    padding: 5px 8px;
    border-bottom: 1px solid var(--border);
  }
  .mini-row:last-child {
    border-bottom: none;
  }
  .mini-head {
    font-weight: 600;
    color: var(--text-faint);
  }
  .mini-row .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .file-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 240px;
    overflow-y: auto;
  }
  .file-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 4px 8px;
    border-radius: var(--radius-sm);
  }
  .file-list li:hover {
    background: var(--surface-3);
  }
  .file-path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .file-size {
    flex-shrink: 0;
    font-size: 12px;
  }
</style>
