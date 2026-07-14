<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listMonitors, createMonitor } from '../lib/api/monitors';
  import type { MonitorListItem } from '../lib/models';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';

  let monitors = $state<MonitorListItem[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showForm = $state(false);

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

  async function submit() {
    if (!projectId || !name || !target) return;
    saving = true;
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

  $effect(() => { if (projectId) void load(); });
</script>

<AppShell>
  <div class="page">
    <header>
      <h1>Uptime</h1>
      {#if canWrite}
        <button onclick={() => (showForm = !showForm)}>{showForm ? 'Cancel' : 'New monitor'}</button>
      {/if}
    </header>

    {#if showForm}
      <div class="form">
        <input placeholder="Name" bind:value={name} />
        <select bind:value={kind}>
          <option value="http">HTTP(S)</option>
          <option value="tcp">TCP</option>
        </select>
        {#if kind === 'http'}
          <input placeholder="https://example.com/health" bind:value={target} />
          <select bind:value={method}>
            <option>GET</option><option>POST</option><option>HEAD</option>
          </select>
        {:else}
          <input placeholder="host:port (e.g. db.example.com:5432)" bind:value={target} />
        {/if}
        <input type="number" min="30" bind:value={interval} /> <span>sec</span>
        <input placeholder="Webhook URL (optional)" bind:value={webhook} />
        <button disabled={saving} onclick={submit}>{saving ? 'Saving…' : 'Create'}</button>
      </div>
    {/if}

    {#if error}<p class="err">{error}</p>{/if}
    {#if loading}
      <p>Loading…</p>
    {:else if monitors.length === 0}
      <p class="empty">No monitors yet.</p>
    {:else}
      <table>
        <thead><tr><th>Name</th><th>Target</th><th>Status</th><th>Uptime 24h</th><th>Latency</th><th>Checked</th></tr></thead>
        <tbody>
          {#each monitors as m (m.id)}
            <tr class="row" onclick={() => push(`/monitors/${m.id}`)}>
              <td>{m.name} <span class="kind">{m.kind}</span></td>
              <td class="mono">{m.target}</td>
              <td><StatusPill status={m.status} /></td>
              <td>{m.uptime_24h == null ? '—' : `${m.uptime_24h.toFixed(1)}%`}</td>
              <td>{m.last_response_time_ms == null ? '—' : `${m.last_response_time_ms} ms`}</td>
              <td>{m.last_checked_at ? new Date(m.last_checked_at).toLocaleTimeString() : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</AppShell>

<style>
  .page { padding: 20px; }
  header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px; }
  .form { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-bottom: 16px;
    padding: 12px; border: 1px solid var(--border); border-radius: var(--radius); }
  input, select { padding: 6px 8px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); color: var(--text); }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 9px 10px; border-bottom: 1px solid var(--border); font-size: 13.5px; }
  .row { cursor: pointer; }
  .row:hover { background: var(--surface-2); }
  .mono { font-family: ui-monospace, monospace; font-size: 12.5px; color: var(--text-muted); }
  .kind { font-size: 11px; color: var(--text-faint); text-transform: uppercase; margin-left: 6px; }
  .err { color: #b42318; }
  .empty { color: var(--text-faint); }
</style>
