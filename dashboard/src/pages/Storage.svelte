<script lang="ts">
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import { getAdminStorage, getTierPolicy, setTierPolicy } from '../lib/api/admin';
  import type { StorageReport, TierPolicy } from '../lib/api/admin';
  import Button from '../lib/components/ui/Button.svelte';
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

  // Rotation policy. Loaded separately from the storage report and allowed to
  // fail on its own: the endpoint requires org:manage in EVERY org, so an admin
  // of one tenant gets a 403 here while the storage report above still renders.
  // Treating that as a page-level error would break Storage for them entirely.
  let policy = $state<TierPolicy | null>(null);
  let policyError = $state<string | null>(null);
  let policyBusy = $state(false);
  let hotDaysInput = $state('');
  let policySaved = $state(false);

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

  async function loadPolicy() {
    policyError = null;
    try {
      policy = await getTierPolicy();
      hotDaysInput = String(policy.effective_hot_days);
    } catch (e) {
      policy = null;
      policyError = (e as Error).message;
    }
  }

  async function savePolicy(next: number | null) {
    policyBusy = true;
    policyError = null;
    policySaved = false;
    try {
      policy = await setTierPolicy(next);
      hotDaysInput = String(policy.effective_hot_days);
      policySaved = true;
    } catch (e) {
      policyError = (e as Error).message;
    } finally {
      policyBusy = false;
    }
  }

  // Parsed once so the button's disabled state and the submit path can never
  // disagree about whether the input is valid.
  const parsedHotDays = $derived.by(() => {
    const t = hotDaysInput.trim();
    if (!/^\d+$/.test(t)) return null;
    const n = Number(t);
    return Number.isSafeInteger(n) ? n : null;
  });
  const hotDaysValid = $derived(
    parsedHotDays !== null && policy !== null && parsedHotDays >= policy.min_hot_days,
  );
  const wouldLower = $derived(
    policy !== null && parsedHotDays !== null && parsedHotDays < policy.effective_hot_days,
  );

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const u = ['KB', 'MB', 'GB', 'TB'];
    let v = n / 1024, i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(1)} ${u[i]}`;
  }

  $effect(() => {
    void load();
    void loadPolicy();
  });
</script>

<AdminShell requireProject={false}>
  <div class="storage">
    <header class="head">
      <div>
        <h1 class="page-title">Storage</h1>
        <!-- Scoped to the orgs you manage, and estimated (rows × avg width) —
             not the physical database size, which would leak other tenants'
             volume. The old "deployment-wide" wording described the pre-scoping
             behaviour and made the smaller number look like data loss. -->
        <p class="sub muted">
          Estimated storage across the organisations you manage, with per-app hot/cold
          record counts.
        </p>
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
        <StatTile
          label="Estimated size"
          value={fmtBytes(rep.database.total_bytes)}
          tone="primary"
        />
        <StatTile label="Tables" value={rep.database.tables.length} />
        <StatTile label="Apps" value={rep.apps.length} />
      </StatTiles>

      <div class="section">
        <Card title="Cold-tier rotation">
          {#if policyError}
            <!-- Shown inline, not as a page error: this endpoint needs org:manage in
                 every org, so a single-tenant admin legitimately gets a 403 while the
                 rest of this page still works for them. -->
            <p class="policy-denied muted">{policyError}</p>
          {:else if policy}
            {@const pol = policy}
            <p class="muted policy-lede">
              Data older than this moves out of Postgres into Parquet. It stays
              readable — queries span both tiers — but it no longer occupies
              database storage.
            </p>

            <div class="policy-row">
              <label class="policy-field">
                <span class="policy-label">Rotation age (days)</span>
                <input
                  class="policy-input"
                  type="number"
                  min={pol.min_hot_days}
                  step="1"
                  bind:value={hotDaysInput}
                  disabled={policyBusy}
                />
              </label>
              <Button
                variant="primary"
                disabled={!hotDaysValid || policyBusy}
                onclick={() => savePolicy(parsedHotDays)}
              >
                {policyBusy ? 'Saving…' : 'Apply'}
              </Button>
              {#if pol.overridden}
                <Button variant="secondary" disabled={policyBusy} onclick={() => savePolicy(null)}>
                  Revert to default ({pol.configured_hot_days}d)
                </Button>
              {/if}
            </div>

            {#if parsedHotDays !== null && !hotDaysValid}
              <p class="policy-warn" role="alert">
                Must be a whole number of days, at least {pol.min_hot_days}. A smaller
                value would put the cutoff at or after now and tier partitions that are
                still being written to.
              </p>
            {:else if wouldLower}
              <!-- The asymmetry is the single most important thing on this page.
                   Lowering acts on the next cycle and cannot be undone by raising
                   the number back. -->
              <p class="policy-warn" role="alert">
                <Icon name="triangle-alert" size={14} />
                Lowering this is one-way. On its next cycle the tier worker will export
                and then drop everything between {parsedHotDays} and
                {pol.effective_hot_days} days old. Raising the number afterwards does
                not bring it back into Postgres — that needs a restore from cold.
              </p>
            {/if}

            {#if policySaved}
              <p class="policy-ok">Saved. Takes effect on the tier worker's next cycle.</p>
            {/if}

            <dl class="policy-facts">
              <div>
                <dt>In force</dt>
                <dd>{pol.effective_hot_days} days{pol.overridden ? '' : ' (default)'}</dd>
              </div>
              <div>
                <dt>Configured</dt>
                <dd>{pol.configured_hot_days} days (TIER_HOT_DAYS)</dd>
              </div>
            </dl>

            {#if pol.follows_on_restart.length > 0}
              <details class="policy-detail">
                <summary>Not every component picks this up immediately</summary>
                <p class="muted">
                  Applies without a restart: {pol.follows_immediately.join('; ')}. Still
                  reading start-time configuration, and so able to disagree about where
                  the boundary is until restarted:
                </p>
                <ul class="muted">
                  {#each pol.follows_on_restart as c (c)}
                    <li>{c}</li>
                  {/each}
                </ul>
              </details>
            {/if}

            {#if pol.pins.length > 0}
              <div class="policy-pins">
                <h3 class="pins-title">Restored ranges held in Postgres</h3>
                <p class="muted">
                  Each pin keeps a restored range from being re-tiered. Without one, a
                  restore is undone on the next cycle.
                </p>
                <ul class="pin-list">
                  {#each pol.pins as pin (pin.id)}
                    <li class:expired={pin.expired}>
                      <code>{pin.table_name}</code>
                      {new Date(pin.range_start).toISOString().slice(0, 10)} →
                      {new Date(pin.range_end).toISOString().slice(0, 10)}
                      <span class="muted">
                        {pin.expired ? 'expired' : 'until'}
                        {new Date(pin.expires_at).toISOString().slice(0, 10)}
                      </span>
                      {#if pin.reason}<span class="muted">— {pin.reason}</span>{/if}
                    </li>
                  {/each}
                </ul>
              </div>
            {/if}
          {:else}
            <div class="center"><Spinner size={20} /></div>
          {/if}
        </Card>
      </div>

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
                  <th>Org</th>
                  <th>Project</th>
                  <th>App</th>
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
                      <!-- The disclosure chevron leads the row, so it stays in
                           the first cell even though what expands below is the
                           app's breakdown. -->
                      <div class="name-cell">
                        <span class="chevron" class:open={openApp[a.app_id]}>
                          <Icon name="chevron-right" size={14} />
                        </span>
                        <span class="cell-muted">{a.org_name}</span>
                      </div>
                    </td>
                    <!-- Empty only for a report cached by a build that predates
                         project_name; the next refresh fills it in. -->
                    <td><span class="cell-muted">{a.project_name || '—'}</span></td>
                    <td><span class="name">{a.app_name}</span></td>
                    <td class="num">{a.hot_rows_total.toLocaleString()}</td>
                    <td class="num">{a.cold_rows_total.toLocaleString()}</td>
                    <td class="num">{fmtBytes(a.cold_bytes_total)}</td>
                    <td class="num">{fmtBytes(a.estimated_hot_bytes_total)}</td>
                  </tr>
                  {#if openApp[a.app_id]}
                    <tr class="expand-row">
                      <td colspan="7" style="background: var(--surface-2); white-space: normal; cursor: default;">
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

                          <!-- Show the true total, not the page size: the API
                               truncates the list, so `cold_files.length` caps
                               out and silently reads as "that's all of them". -->
                          <h4 class="expand-title">Cold Parquet files ({a.cold_files_total})</h4>
                          {#if a.cold_files_total === 0}
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
                            {#if a.cold_files_total > a.cold_files.length}
                              <p class="faint">
                                Showing the first {a.cold_files.length} of {a.cold_files_total} files.
                              </p>
                            {/if}
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
</AdminShell>

<style>
  .policy-lede { margin: 0 0 14px; }
  .policy-row { display: flex; align-items: flex-end; gap: 10px; flex-wrap: wrap; }
  .policy-field { display: flex; flex-direction: column; gap: 5px; }
  .policy-label { font-size: 12px; color: var(--text-muted); }
  .policy-input {
    width: 120px;
    padding: 7px 9px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
  }
  .policy-warn {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    margin: 12px 0 0;
    padding: 9px 11px;
    border: 1px solid var(--warning, var(--border-strong));
    border-radius: var(--radius);
    color: var(--text);
    font-size: 13px;
    line-height: 1.5;
  }
  .policy-ok { margin: 10px 0 0; font-size: 13px; color: var(--success, var(--text)); }
  .policy-denied { margin: 0; }
  .policy-facts {
    display: flex;
    gap: 26px;
    margin: 16px 0 0;
    flex-wrap: wrap;
  }
  .policy-facts dt { font-size: 11px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-muted); }
  .policy-facts dd { margin: 3px 0 0; font-size: 14px; }
  .policy-detail { margin-top: 14px; font-size: 13px; }
  .policy-detail summary { cursor: pointer; color: var(--text-muted); }
  .policy-detail ul { margin: 6px 0 0 18px; }
  .policy-pins { margin-top: 18px; }
  .pins-title { margin: 0 0 4px; font-size: 14px; }
  .pin-list { margin: 8px 0 0; padding-left: 18px; font-size: 13px; }
  .pin-list li.expired { opacity: 0.55; }

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
