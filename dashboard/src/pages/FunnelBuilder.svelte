<script lang="ts">
  import { untrack } from 'svelte';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import FunnelChart from '../lib/components/FunnelChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { topEvents } from '../lib/api/events';
  import {
    computeFunnel,
    listSavedFunnels,
    saveFunnel,
    updateFunnel,
    deleteFunnel,
  } from '../lib/api/funnels';
  import { errorMessage } from '../lib/api/client';
  import { formatPercent } from '../lib/utils/format';
  import type { TopEvent, FunnelResult, SavedFunnel } from '../lib/models';

  let sinceDays = $state(30);

  let available = $state<TopEvent[]>([]);
  let steps = $state<string[]>([]);
  let picked = $state('');
  let result = $state<FunnelResult | null>(null);

  let loadingEvents = $state(true);
  let computing = $state(false);
  let error = $state<string | null>(null);
  let refreshing = $state(false);

  let saved = $state<SavedFunnel[]>([]);
  let loadedId = $state<string | null>(null);
  const canWrite = $derived(sessionStore.can('funnel:write'));

  // Save / edit dialog
  let showDetailsDialog = $state(false);
  let dialogMode = $state<'create' | 'edit'>('create');
  let dialogName = $state('');
  let dialogDesc = $state('');
  let savingDialog = $state(false);

  // Delete confirmation
  let showDeleteConfirm = $state(false);
  let pendingDelete = $state<SavedFunnel | null>(null);
  let deleting = $state(false);

  // Saved-funnel search
  let funnelSearch = $state('');
  const filteredFunnels = $derived.by(() => {
    const q = funnelSearch.trim().toLowerCase();
    if (!q) return saved;
    return saved.filter((f) => f.name.toLowerCase().includes(q));
  });

  async function loadSaved(aid: string) {
    try {
      saved = await listSavedFunnels(aid);
    } catch {
      saved = [];
    }
  }

  function openSaveDialog() {
    if (steps.length < 2) return;
    dialogMode = 'create';
    dialogName = '';
    dialogDesc = '';
    showDetailsDialog = true;
  }

  function openUpdateDialog() {
    if (!loadedId || steps.length < 2) return;
    const current = saved.find((f) => f.id === loadedId);
    dialogMode = 'edit';
    dialogName = current?.name ?? '';
    dialogDesc = current?.description ?? '';
    showDetailsDialog = true;
  }

  function closeDialog() {
    showDetailsDialog = false;
  }

  async function submitDialog() {
    const aid = sessionStore.currentAppId;
    const name = dialogName.trim();
    if (!aid || !name || steps.length < 2) return;
    savingDialog = true;
    const body = {
      name,
      description: dialogDesc.trim() || undefined,
      steps: [...steps],
    };
    try {
      if (dialogMode === 'edit' && loadedId) {
        await updateFunnel(aid, loadedId, body);
        toastStore.success('Funnel updated.');
      } else {
        const created = await saveFunnel(aid, body);
        loadedId = created.id;
        toastStore.success('Funnel saved.');
      }
      showDetailsDialog = false;
      await loadSaved(aid);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      savingDialog = false;
    }
  }

  function loadFunnel(f: SavedFunnel) {
    steps = [...f.steps];
    loadedId = f.id;
    const aid = sessionStore.currentAppId;
    if (aid) void compute(aid, sinceDays);
  }

  async function duplicateFunnel(f: SavedFunnel) {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    try {
      await saveFunnel(aid, {
        name: `Copy of ${f.name}`,
        description: f.description ?? undefined,
        steps: [...f.steps],
      });
      await loadSaved(aid);
      toastStore.success('Funnel duplicated.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    }
  }

  function openDeleteConfirm(f: SavedFunnel) {
    pendingDelete = f;
    showDeleteConfirm = true;
  }

  function cancelDelete() {
    showDeleteConfirm = false;
    pendingDelete = null;
  }

  async function confirmDelete() {
    const aid = sessionStore.currentAppId;
    const f = pendingDelete;
    if (!aid || !f) return;
    deleting = true;
    try {
      await deleteFunnel(aid, f.id);
      if (loadedId === f.id) loadedId = null;
      await loadSaved(aid);
      toastStore.success('Funnel deleted.');
      showDeleteConfirm = false;
      pendingDelete = null;
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      deleting = false;
    }
  }

  async function compute(aid: string, days: number) {
    if (steps.length < 2) return;
    computing = true;
    error = null;
    try {
      result = await computeFunnel(aid, [...steps], days);
    } catch (err) {
      error = errorMessage(err);
      result = null;
    } finally {
      computing = false;
    }
  }

  async function loadEvents(aid: string) {
    loadingEvents = true;
    error = null;
    steps = [];
    result = null;
    try {
      available = await topEvents(aid, { since_days: 90, limit: 50 });
      picked = available[0]?.name ?? '';
      if (available.length >= 2) {
        // Prefill the first 3 (or 2) events and compute so the page isn't empty.
        const n = Math.min(3, available.length);
        steps = available.slice(0, n).map((e) => e.name);
        void compute(aid, sinceDays);
      }
    } catch (err) {
      error = errorMessage(err);
      available = [];
    } finally {
      loadingEvents = false;
    }
  }

  // Load available events whenever the current app changes.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    if (aid) {
      void loadEvents(aid);
      void loadSaved(aid);
    }
  });

  // Recompute automatically when the date range changes (only tracks sinceDays).
  $effect(() => {
    const days = sinceDays;
    untrack(() => {
      const aid = sessionStore.currentAppId;
      if (aid && steps.length >= 2) void compute(aid, days);
    });
  });

  function addStep() {
    if (picked) steps = [...steps, picked];
  }

  function removeStep(i: number) {
    steps = steps.filter((_, idx) => idx !== i);
  }

  function onCompute() {
    const aid = sessionStore.currentAppId;
    if (aid) void compute(aid, sinceDays);
  }

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void loadEvents(aid);
  }

  async function refresh() {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    refreshing = true;
    try {
      await Promise.all([loadEvents(aid), loadSaved(aid)]);
    } finally {
      refreshing = false;
    }
  }

  const overallConv = $derived(result ? (result.steps.at(-1)?.conv_from_start ?? 0) : 0);
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Funnels</h1>
      <p class="muted sub">Define an ordered set of events and track conversion & drop-off between steps.</p>
    </div>
    <div class="controls">
      <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
      <RefreshButton onclick={refresh} loading={refreshing} />
    </div>
  </div>

  {#if loadingEvents}
    <Card><div class="center"><Spinner size={22} /></div></Card>
  {:else if error && available.length === 0}
    <Card>
      <EmptyState title="Couldn't load events" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>Retry</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if available.length === 0}
    <Card>
      <EmptyState
        title="No events yet"
        description="Send events from your SDK to start building conversion funnels."
        icon="chart-column"
      />
    </Card>
  {:else}
    {#if saved.length > 0}
      <Card title="Saved funnels">
        <div class="saved-search">
          <SearchInput bind:value={funnelSearch} placeholder="Search funnels…" width="280px" />
        </div>
        {#if filteredFunnels.length === 0}
          <p class="muted empty-steps">No funnels match “{funnelSearch}”.</p>
        {:else}
          <ul class="saved-list">
            {#each filteredFunnels as f (f.id)}
              <li class="saved-item" class:active={f.id === loadedId}>
                <button class="load" type="button" onclick={() => loadFunnel(f)} title="Load this funnel">
                  <span class="sf-name truncate">{f.name}</span>
                  <span class="sf-meta">{f.steps.length} steps{#if f.created_by_name} · {f.created_by_name}{/if}</span>
                </button>
                <div class="sf-actions">
                  <button type="button" title="Duplicate" onclick={() => duplicateFunnel(f)}><Icon name="copy" size={14} /></button>
                  {#if canWrite}
                    <button type="button" title="Delete" onclick={() => openDeleteConfirm(f)}><Icon name="x" size={14} /></button>
                  {/if}
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
    {/if}

    <div class="grid">
      <Card title="Builder">
        {#if steps.length === 0}
          <p class="muted empty-steps">Add at least two steps to compute a funnel.</p>
        {:else}
          <ol class="steps">
            {#each steps as step, i (i)}
              <li class="step">
                <span class="snum">{i + 1}</span>
                <span class="sname mono truncate">{step}</span>
                <button class="remove" type="button" title="Remove step" onclick={() => removeStep(i)}><Icon name="x" size={14} /></button>
              </li>
            {/each}
          </ol>
        {/if}

        <div class="add-row">
          <select class="picker" bind:value={picked} aria-label="Event to add">
            {#each available as ev (ev.name)}
              <option value={ev.name}>{ev.name}</option>
            {/each}
          </select>
          <Button variant="secondary" size="sm" onclick={addStep}>Add step</Button>
        </div>

        <div class="compute-row">
          <Button variant="primary" onclick={onCompute} disabled={steps.length < 2} loading={computing}>
            Compute funnel
          </Button>
          {#if canWrite}
            {#if loadedId}
              <Button variant="secondary" size="sm" onclick={openUpdateDialog} disabled={steps.length < 2}>Update</Button>
              <Button variant="secondary" size="sm" onclick={openSaveDialog} disabled={steps.length < 2}>Save as new</Button>
            {:else}
              <Button variant="secondary" size="sm" onclick={openSaveDialog} disabled={steps.length < 2}>Save template</Button>
            {/if}
          {/if}
          {#if steps.length < 2}
            <span class="faint hint">Need at least 2 steps</span>
          {/if}
        </div>
      </Card>

      <Card title="Results">
        {#if computing && !result}
          <div class="center"><Spinner size={22} /></div>
        {:else if error && !result}
          <EmptyState title="Couldn't compute funnel" description={error} icon="triangle-alert">
            {#snippet action()}
              <Button variant="secondary" onclick={onCompute}>Retry</Button>
            {/snippet}
          </EmptyState>
        {:else if result}
          <div class="summary" class:updating={computing}>
            <div class="sum-item">
              <span class="sum-label section-label">Entered</span>
              <span class="sum-val">{result.total_entered.toLocaleString()}</span>
            </div>
            <div class="sum-item">
              <span class="sum-label section-label">Overall conversion</span>
              <span class="sum-val accent">{formatPercent(overallConv)}</span>
            </div>
            {#if computing}
              <span class="updating-tag"><Spinner size={13} /> Updating…</span>
            {/if}
          </div>
          <div class="chart-wrap">
            <FunnelChart result={result} />
          </div>
        {:else}
          <p class="muted empty-steps">Compute a funnel to see conversion & drop-off.</p>
        {/if}
      </Card>
    </div>
  {/if}

  <Modal
    bind:open={showDetailsDialog}
    title={dialogMode === 'edit' ? 'Edit funnel template' : 'Save funnel template'}
    onclose={closeDialog}
  >
    <div class="dialog-form">
      <Input label="Name" bind:value={dialogName} placeholder="Signup flow" required />
      <div class="ta-field">
        <label class="ta-label" for="funnel-desc">Description <span class="ta-opt">optional</span></label>
        <textarea
          id="funnel-desc"
          class="ta"
          bind:value={dialogDesc}
          rows="3"
          placeholder="What this funnel tracks…"
        ></textarea>
      </div>
      <p class="dialog-hint">
        {steps.length} step{steps.length === 1 ? '' : 's'} will be saved from the builder.
      </p>
    </div>
    {#snippet footer()}
      <Button variant="secondary" onclick={closeDialog} disabled={savingDialog}>Cancel</Button>
      <Button
        variant="primary"
        onclick={submitDialog}
        disabled={!dialogName.trim() || steps.length < 2}
        loading={savingDialog}
      >
        {dialogMode === 'edit' ? 'Save changes' : 'Save template'}
      </Button>
    {/snippet}
  </Modal>

  <ConfirmDialog
    bind:open={showDeleteConfirm}
    title="Delete funnel?"
    message={pendingDelete
      ? `“${pendingDelete.name}” will be permanently removed. This can't be undone.`
      : ''}
    confirmLabel="Delete"
    danger
    loading={deleting}
    onconfirm={confirmDelete}
    oncancel={cancelDelete}
  />
</AppShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
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
    max-width: 620px;
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 200px;
  }
  .grid {
    display: grid;
    grid-template-columns: minmax(300px, 360px) 1fr;
    gap: 18px;
    align-items: start;
  }

  .empty-steps {
    font-size: 13px;
  }

  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .step {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 9px 7px 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .snum {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 11px;
    font-weight: 680;
    flex-shrink: 0;
  }
  .sname {
    flex: 1;
    min-width: 0;
    font-size: 12.5px;
  }
  .remove {
    background: none;
    border: none;
    color: var(--text-faint);
    font-size: 12px;
    line-height: 1;
    padding: 3px 5px;
    border-radius: var(--radius-sm);
    flex-shrink: 0;
  }
  .remove:hover {
    color: var(--error);
    background: var(--error-soft);
  }

  .add-row {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  .picker {
    flex: 1;
    min-width: 0;
    padding: 7px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    color: var(--text);
    font-size: 12.5px;
    font-family: inherit;
  }
  .picker:focus {
    outline: none;
    border-color: var(--primary-border);
  }

  .compute-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 14px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .hint {
    font-size: 11.5px;
  }

  .summary {
    display: flex;
    align-items: flex-end;
    gap: 28px;
    margin-bottom: 20px;
    transition: opacity 0.15s ease;
  }
  .summary.updating {
    opacity: 0.75;
  }
  .sum-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .sum-label {
    font-size: 10.5px;
  }
  .sum-val {
    font-size: 22px;
    font-weight: 660;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .sum-val.accent {
    color: var(--primary);
  }
  .updating-tag {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    color: var(--text-muted);
    margin-bottom: 2px;
  }
  .chart-wrap {
    padding-top: 4px;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }

  .saved-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .saved-item {
    display: flex;
    align-items: center;
    gap: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
  }
  .saved-item.active {
    border-color: var(--primary-border);
  }
  .saved-item .load {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 7px 10px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
  }
  .sf-name {
    font-size: 13px;
    color: var(--text);
    font-weight: 560;
    max-width: 220px;
  }
  .sf-meta {
    font-size: 11px;
    color: var(--text-faint);
  }
  .sf-actions {
    display: flex;
    gap: 2px;
    padding-right: 6px;
  }
  .sf-actions button {
    background: none;
    border: none;
    color: var(--text-faint);
    padding: 4px;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .sf-actions button:hover {
    color: var(--text);
    background: var(--surface-3);
  }

  .saved-search {
    margin-bottom: 12px;
  }

  .dialog-form {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .ta-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ta-label {
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
  }
  .ta-opt {
    color: var(--text-faint);
    font-weight: 400;
  }
  .ta {
    width: 100%;
    padding: 10px 13px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-family: inherit;
    font-size: 13.5px;
    line-height: 1.5;
    resize: vertical;
    min-height: 62px;
    transition: border-color 0.14s ease, box-shadow 0.14s ease;
  }
  .ta:focus {
    outline: none;
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  .ta::placeholder {
    color: var(--text-faint);
  }
  .dialog-hint {
    font-size: 12px;
    color: var(--text-faint);
    margin: 0;
  }
</style>
