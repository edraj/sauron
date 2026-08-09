<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
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
  // monitors.rs:99,191,223 authorize at the project.
  const writeLock = $derived(lockedBy('monitor:write', { project: projectId, level: 'project' }));

  // Cached view (lib/stores/cached-view.svelte.ts): the list paints instantly when
  // you come back from a monitor's detail page, then refreshes behind a spinner.
  // Re-exposed under the names the template already used, so the markup is
  // unchanged apart from the refresh control.
  const monitorsView = new CachedView<MonitorListItem[]>();

  const monitors = $derived(monitorsView.data ?? []);
  const revalidating = $derived(monitorsView.revalidating);
  const loading = $derived(monitorsView.loading);

  // The create form reports failures through the same banner as the list's own
  // load error, and a `$derived` cannot be assigned to — so the form keeps its
  // own state and the template's `error` is the two folded together.
  let formError = $state<string | null>(null);
  const error = $derived(formError ?? monitorsView.error);

  /**
   * `force` bypasses the fresh-window short-circuit: an explicit Refresh, and the
   * re-list after a successful create, both mean "go to the network now".
   *
   * `scopeKey` rides in the key under the project-wide rule even though this
   * particular request is NOT environment-scoped: `/v1/projects/{id}/monitors` is
   * listed in `api/scope.ts`'s `PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID`, so the
   * interceptor never attaches `environment_id` to it. Over-keying costs at worst
   * one extra fetch; under-keying serves one scope's rows as another's.
   */
  async function load(force = false) {
    const pid = projectId;
    if (!pid) return;
    formError = null;
    await monitorsView.load(
      viewKey('monitors.list', pid, sessionStore.scopeKey),
      () => listMonitors(pid),
      force,
    );
  }

  async function refresh() {
    if (!projectId) return;
    refreshing = true;
    try { await load(true); }
    finally { refreshing = false; }
  }

  function openForm() { formError = null; showForm = true; }
  function closeForm() { showForm = false; }

  async function submit() {
    if (!projectId || !name || !target) return;
    saving = true; formError = null;
    try {
      await createMonitor(projectId, {
        name, kind, target, method: kind === 'http' ? method : undefined,
        interval_seconds: interval, webhook_url: webhook || undefined,
      });
      showForm = false; name = ''; target = ''; webhook = '';
      // force: the row we just created must not be hidden behind a cache entry
      // written a moment before the POST.
      await load(true);
    } catch (e) { formError = (e as Error).message; }
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

  $effect(() => {
    // Touched explicitly rather than left to the incidental read inside `load()`:
    // it is part of the cache key, so the effect that fills that key has to
    // re-run when it changes.
    sessionStore.scopeKey;
    // `idle()` rather than deriving `!!projectId && view.loading`: that
    // spelling left `loading` false with no data, so the template fell
    // through to a confident "No monitors yet" while the project selection
    // was merely absent. `idle()` means "nothing to load", which is the
    // truth, and it also cancels any in-flight load from the previous project.
    if (projectId) void load();
    else monitorsView.idle();
  });
</script>

<AppShell>
  <div class="mons">
    <header class="head">
      <div>
        <h1 class="page-title">Uptime</h1>
        <p class="sub muted">Track availability and latency for your HTTP and TCP endpoints.</p>
      </div>
      <div class="controls">
        {#if !showForm}
          <Button variant="primary" lockedReason={writeLock} onclick={openForm}>New monitor</Button>
        {/if}
        <!--
          Spins for a background revalidate too, not just an explicit click: that
          spinner IS the "showing cached rows, fetching fresh" hint, and without it
          the instant paint is indistinguishable from live data.
        -->
        <RefreshButton onclick={refresh} loading={refreshing || revalidating} />
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
          <Button
            variant="primary"
            loading={saving}
            disabled={!name || !target}
            lockedReason={writeLock}
            onclick={submit}
          >
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
          {#if !showForm}
            <Button variant="primary" lockedReason={writeLock} onclick={openForm}>New monitor</Button>
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
