<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listMonitors, createMonitor } from '../lib/api/monitors';
  import { MONITOR_INTERVALS } from '../lib/constants/monitorIntervals';
  import type { MonitorListItem } from '../lib/models';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';

  let monitors = $state<MonitorListItem[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showForm = $state(false);
  let refreshing = $state(false);

  // create form
  let name = $state('');
  let kind = $state<'http' | 'tcp'>('http');
  let target = $state('');
  let method = $state('GET');
  let interval = $state(60);
  let webhook = $state('');
  let saving = $state(false);

  const projectId = $derived(sessionStore.currentProjectId);
  const canWrite = $derived(sessionStore.can('monitor:write', { project: projectId }));

  async function load() {
    if (!projectId) { loading = false; return; }
    loading = true; error = null;
    try { monitors = await listMonitors(projectId); }
    catch (e) { error = (e as Error).message; }
    finally { loading = false; }
  }

  async function refresh() {
    if (!projectId) return;
    refreshing = true;
    try { await load(); }
    finally { refreshing = false; }
  }

  function openForm() { error = null; showForm = true; }
  function closeForm() { showForm = false; }

  async function submit() {
    if (!projectId || !name || !target) return;
    saving = true; error = null;
    try {
      await createMonitor(projectId, {
        name, kind, target, method: kind === 'http' ? method : undefined,
        interval_seconds: interval, webhook_url: webhook || undefined,
      });
      showForm = false; name = ''; target = ''; webhook = '';
      await load();
    } catch (e) { error = (e as Error).message; }
    finally { saving = false; }
  }

  const fmtTime = (iso: string) =>
    new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

  // Color the 24h uptime figure so health reads at a glance, independent of the
  // "up right now" status pill. Applied inline so it wins over DataTable's own
  // `tbody td` color rule without a specificity fight.
  function uptimeColor(v: number | null): string {
    if (v == null) return '';
    if (v >= 99) return 'var(--success)';
    if (v >= 95) return 'var(--warning)';
    return 'var(--error)';
  }

  $effect(() => { if (projectId) void load(); });
</script>

<AppShell>
  <div class="mons">
    <header class="head">
      <div>
        <h1 class="page-title">Uptime</h1>
        <p class="sub muted">Track availability and latency for your HTTP and TCP endpoints.</p>
      </div>
      <div class="controls">
        {#if canWrite && !showForm}
          <Button variant="primary" onclick={openForm}>New monitor</Button>
        {/if}
        <RefreshButton onclick={refresh} loading={refreshing} />
      </div>
    </header>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    {#if showForm}
      <Card title="New monitor">
        <div class="form-grid">
          <Input label="Name" bind:value={name} placeholder="API health check" required />

          <div class="field">
            <label class="lbl" for="mon-kind">Type</label>
            <div class="control select">
              <select id="mon-kind" bind:value={kind}>
                <option value="http">HTTP(S)</option>
                <option value="tcp">TCP</option>
              </select>
              <span class="affix"><Icon name="chevron-down" size={15} /></span>
            </div>
          </div>

          {#if kind === 'http'}
            <div class="span-2">
              <Input label="URL" bind:value={target} placeholder="https://example.com/health" required />
            </div>
            <div class="field">
              <label class="lbl" for="mon-method">Method</label>
              <div class="control select">
                <select id="mon-method" bind:value={method}>
                  <option>GET</option><option>POST</option><option>HEAD</option>
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>
          {:else}
            <div class="span-2">
              <Input label="Host & port" bind:value={target} placeholder="db.example.com:5432" required />
            </div>
          {/if}

          <div class="field">
            <label class="lbl" for="mon-interval">Interval</label>
            <div class="control select">
              <select id="mon-interval" bind:value={interval}>
                {#each MONITOR_INTERVALS as opt (opt.seconds)}
                  <option value={opt.seconds}>{opt.label}</option>
                {/each}
              </select>
              <span class="affix"><Icon name="chevron-down" size={15} /></span>
            </div>
          </div>

          <div class="span-2">
            <Input
              label="Webhook URL"
              bind:value={webhook}
              placeholder="https://hooks.example.com/…"
              hint="Optional — notified when this monitor changes state."
            />
          </div>
        </div>

        <div class="form-foot">
          <Button variant="ghost" onclick={closeForm}>Cancel</Button>
          <Button variant="primary" loading={saving} disabled={!name || !target} onclick={submit}>
            Create monitor
          </Button>
        </div>
      </Card>
    {/if}

    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if monitors.length === 0}
      <EmptyState
        title="No monitors yet"
        description="Add an HTTP or TCP monitor to start tracking uptime, latency, and incidents."
        icon="zap"
      >
        {#snippet action()}
          {#if canWrite && !showForm}
            <Button variant="primary" onclick={openForm}>New monitor</Button>
          {/if}
        {/snippet}
      </EmptyState>
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th>Name</th>
            <th>Target</th>
            <th>Status</th>
            <th class="num">Uptime 24h</th>
            <th class="num">Latency</th>
            <th class="num">Checked</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each monitors as m (m.id)}
            <tr class="clickable" onclick={() => push(`/monitors/${m.id}`)}>
              <td>
                <div class="name-cell">
                  <span class="name">{m.name}</span>
                  <span class="kind">{m.kind}</span>
                </div>
              </td>
              <td><span class="cell-mono cell-muted target" title={m.target}>{m.target}</span></td>
              <td><StatusPill status={m.status} /></td>
              <td class="num" style:color={uptimeColor(m.uptime_24h)}>
                {#if m.uptime_24h == null}<span class="faint">—</span>{:else}{m.uptime_24h.toFixed(1)}%{/if}
              </td>
              <td class="num">
                {#if m.last_response_time_ms == null}<span class="faint">—</span>{:else}{m.last_response_time_ms} ms{/if}
              </td>
              <td class="num">
                {#if m.last_checked_at}<span class="cell-muted">{fmtTime(m.last_checked_at)}</span>{:else}<span class="faint">—</span>{/if}
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    {/if}
  </div>
</AppShell>

<style>
  .mons {
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
  .controls {
    display: flex;
    align-items: center;
    gap: 10px;
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

  /* --- create form ---------------------------------------------------------- */
  .form-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 15px 16px;
  }
  .span-2 {
    grid-column: 1 / -1;
  }
  .form-foot {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 18px;
  }

  /* Native controls (select / number) styled to match the Input component. */
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
  }
  .control {
    position: relative;
    display: flex;
    align-items: center;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    transition: border-color 0.14s ease, box-shadow 0.14s ease;
  }
  .control:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  .control select {
    flex: 1;
    width: 100%;
    min-width: 0;
    padding: 10px 13px;
    background: transparent;
    border: none;
    color: var(--text);
    outline: none;
  }
  .control.select select {
    appearance: none;
    padding-right: 34px;
    cursor: pointer;
  }
  .affix {
    display: inline-flex;
    align-items: center;
    color: var(--text-faint);
    pointer-events: none;
  }
  .control.select .affix {
    position: absolute;
    right: 11px;
  }

  /* --- table cells ---------------------------------------------------------- */
  .center {
    display: grid;
    place-items: center;
    min-height: 180px;
  }
  .name-cell {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .name {
    font-weight: 560;
  }
  .kind {
    font-size: 10px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 6px;
  }
  .target {
    display: inline-block;
    max-width: 340px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
  }
</style>
