<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import { getMonitor, getMonitorChecks, updateMonitor, deleteMonitor } from '../lib/api/monitors';
  import type { MonitorDetail, MonitorCheck } from '../lib/models';
  import { sessionStore } from '../lib/stores/session.svelte';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';

  let { params }: { params: { id: string } } = $props();

  let detail = $state<MonitorDetail | null>(null);
  let checks = $state<MonitorCheck[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  const canWrite = $derived(
    sessionStore.can('monitor:write', { project: detail?.monitor.project_id }),
  );

  async function load() {
    loading = true; error = null;
    try {
      detail = await getMonitor(params.id);
      checks = await getMonitorChecks(params.id, 24);
    } catch (e) { error = (e as Error).message; }
    finally { loading = false; }
  }

  async function togglePause() {
    if (!detail) return;
    const nextEnabled = detail.monitor.status === 'paused';
    await updateMonitor(params.id, { enabled: nextEnabled });
    await load();
  }

  async function remove() {
    if (!detail) return;
    if (!confirm(`Delete monitor "${detail.monitor.name}"?`)) return;
    await deleteMonitor(params.id);
    push('/monitors');
  }

  const fmtPct = (v: number | null | undefined) => (v == null ? '—' : `${v.toFixed(2)}%`);

  $effect(() => {
    if (params.id) void load();
  });
</script>

<AppShell>
  {#if loading}
    <p class="p">Loading…</p>
  {:else if error}
    <p class="p err">{error}</p>
  {:else if detail}
    <div class="p">
      <a href="#/monitors" class="back">← Uptime</a>
      <header>
        <div>
          <h1>{detail.monitor.name} <StatusPill status={detail.monitor.status} /></h1>
          <p class="mono">{detail.monitor.kind.toUpperCase()} · {detail.monitor.target}</p>
        </div>
        {#if canWrite}
          <div class="actions">
            <button onclick={togglePause}>{detail.monitor.status === 'paused' ? 'Resume' : 'Pause'}</button>
            <button class="danger" onclick={remove}>Delete</button>
          </div>
        {/if}
      </header>

      <div class="tiles">
        <div class="tile"><span>Uptime 24h</span><strong>{fmtPct(detail.uptime.h24)}</strong></div>
        <div class="tile"><span>Uptime 7d</span><strong>{fmtPct(detail.uptime.d7)}</strong></div>
        <div class="tile"><span>Uptime 30d</span><strong>{fmtPct(detail.uptime.d30)}</strong></div>
        <div class="tile"><span>Interval</span><strong>{detail.monitor.interval_seconds}s</strong></div>
      </div>

      <h2>Recent checks</h2>
      <table>
        <thead><tr><th>Time</th><th>Result</th><th>Code</th><th>Latency</th><th>Error</th></tr></thead>
        <tbody>
          {#each checks.slice().reverse().slice(0, 50) as c (c.checked_at)}
            <tr>
              <td>{new Date(c.checked_at).toLocaleString()}</td>
              <td class={c.up ? 'ok' : 'bad'}>{c.up ? 'up' : 'down'}</td>
              <td>{c.status_code ?? '—'}</td>
              <td>{c.response_time_ms == null ? '—' : `${c.response_time_ms} ms`}</td>
              <td class="mono">{c.error ?? ''}</td>
            </tr>
          {/each}
        </tbody>
      </table>

      <h2>Incidents</h2>
      {#if detail.incidents.length === 0}
        <p class="empty">No incidents recorded.</p>
      {:else}
        <table>
          <thead><tr><th>Started</th><th>Resolved</th><th>Cause</th></tr></thead>
          <tbody>
            {#each detail.incidents as i (i.id)}
              <tr>
                <td>{new Date(i.started_at).toLocaleString()}</td>
                <td>{i.resolved_at ? new Date(i.resolved_at).toLocaleString() : 'ongoing'}</td>
                <td>{i.cause}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  {/if}
</AppShell>

<style>
  .p { padding: 20px; }
  .back { color: var(--text-muted); font-size: 13px; }
  header { display: flex; justify-content: space-between; align-items: flex-start; margin: 8px 0 18px; }
  .mono { font-family: ui-monospace, monospace; color: var(--text-muted); font-size: 13px; }
  .actions { display: flex; gap: 8px; }
  .danger { color: #b42318; }
  .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 12px; margin-bottom: 24px; }
  .tile { border: 1px solid var(--border); border-radius: var(--radius); padding: 12px 14px; display: flex; flex-direction: column; gap: 4px; }
  .tile span { font-size: 12px; color: var(--text-faint); }
  .tile strong { font-size: 20px; }
  table { width: 100%; border-collapse: collapse; margin-bottom: 24px; }
  th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--border); font-size: 13px; }
  .ok { color: #16794a; } .bad { color: #b42318; }
  .err { color: #b42318; } .empty { color: var(--text-faint); }
</style>
