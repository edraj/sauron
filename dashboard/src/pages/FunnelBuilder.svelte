<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { untrack } from 'svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import Select from '../lib/components/ui/Select.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Textarea from '../lib/components/ui/Textarea.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import { rangeStore } from '../lib/stores/range.svelte';
  import { type DateRangeValue } from '../lib/models/date-range';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import FunnelChart from '../lib/components/FunnelChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { envelopeStatus } from '../lib/models/freshness';
  import { viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
  import { lockTip } from '../lib/actions/lock-tip';
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

  // The shared selection, falling back to this page's own 30 days.
  let range = $state<DateRangeValue>(rangeStore.effective(30));

  /**
   * Only the CATALOGUE is cached, not the builder.
   *
   * `loadEvents` seeds mutable state the user then edits — `steps`, `picked`,
   * `result`. Caching those would either clobber an edited funnel on return or
   * leave the builder empty on a hit, so the seeding below stays imperative
   * and runs after `load` resolves, which it does for a cache hit and a fetch
   * alike. What is cached is the pure server read: a 90-day top-events list.
   */
  const catalogView = new CachedView<TopEvent[]>();
  const available = $derived(catalogView.data ?? []);
  let steps = $state<string[]>([]);
  let picked = $state('');
  let result = $state<FunnelResult | null>(null);

  const loadingEvents = $derived(catalogView.loading);
  let computing = $state(false);
  let error = $state<string | null>(null);
  // The caught failure itself, so ErrorState can say what happened in words
  // (a `query_timeout` 503 is "took too long"); `error` stays the message.
  let computeFailure = $state<unknown>(null);
  let refreshing = $state(false);

  let saved = $state<SavedFunnel[]>([]);
  let loadedId = $state<string | null>(null);
  // funnels.rs:204,232,257 authorize at the app.
  const writeLock = $derived(
    lockedBy('funnel:write', { app: sessionStore.currentAppId, level: 'app' }),
  );

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
        toastStore.success(t('funnels.toast.updated'));
      } else {
        const created = await saveFunnel(aid, body);
        loadedId = created.id;
        toastStore.success(t('funnels.toast.saved'));
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
    if (aid) void compute(aid, range);
  }

  async function duplicateFunnel(f: SavedFunnel) {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    try {
      await saveFunnel(aid, {
        name: t('funnels.copyOf', { name: f.name }),
        description: f.description ?? undefined,
        steps: [...f.steps],
      });
      await loadSaved(aid);
      toastStore.success(t('funnels.toast.duplicated'));
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
      toastStore.success(t('funnels.toast.deleted'));
      showDeleteConfirm = false;
      pendingDelete = null;
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      deleting = false;
    }
  }

  /**
   * Compute the funnel, polling while the server does.
   *
   * The aggregate moved off the request path — a three-step funnel measured
   * 12.97 s on a 34M-event window, and each step is another pass — so a cold
   * request answers `computing` and the counts follow. `pending` holds the
   * inputs so the poll re-asks for exactly the funnel being waited on, never
   * whatever the builder has been edited to since.
   */
  let pending = $state<{ aid: string; win: DateRangeValue; steps: string[] } | null>(null);

  async function compute(aid: string, win: DateRangeValue) {
    if (steps.length < 2) return;
    const wanted = [...steps];
    computeFailure = null;
    computing = true;
    error = null;
    try {
      const envelope = await computeFunnel(aid, wanted, win);
      const status = envelopeStatus({
        state: envelope.state,
        error: envelope.error,
        hasData: envelope.data !== null,
      });
      if (status.error) {
        computeFailure = null;
        error = status.error;
        result = null;
        pending = null;
      } else if (envelope.data) {
        result = envelope.data;
        pending = null;
      } else {
        // Still computing: keep the previous counts off screen and ask again.
        result = null;
        pending = { aid, win, steps: wanted };
        return; // `computing` stays true — the work is not finished
      }
    } catch (err) {
      error = errorMessage(err);
      computeFailure = err;
      result = null;
      pending = null;
    }
    computing = false;
  }

  $effect(() => {
    const p = pending;
    if (!p) return;
    const id = setTimeout(() => void compute(p.aid, p.win), 1500);
    return () => clearTimeout(id);
  });

  async function loadEvents(aid: string) {
    error = null;
    steps = [];
    result = null;
    // A fixed 90 days, NOT the picker's window: this list is the CATALOGUE
    // of event names to build a funnel from, and narrowing it to the
    // reporting window would hide steps the user wants to measure.
    await catalogView.load(
      viewKey('funnel.catalogue', aid, sessionStore.scopeKey),
      () => topEvents(aid, { since_days: '90', limit: 50 }),
    );
    if (catalogView.error) {
      error = catalogView.error;
      return;
    }
    picked = available[0]?.name ?? '';
    if (available.length >= 2) {
      // Prefill the first 3 (or 2) events and compute so the page isn't empty.
      const n = Math.min(3, available.length);
      steps = available.slice(0, n).map((e) => e.name);
      void compute(aid, range);
    }
  }

  // Load available events whenever the current app or environment changes.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    // (loadSaved calls listSavedFunnels, which is never environment-scoped —
    // saved funnel definitions are app-wide — but loadEvents's topEvents call
    // is, so this effect still needs to key on scopeKey as a whole.)
    sessionStore.scopeKey;
    if (aid) {
      void loadEvents(aid);
      void loadSaved(aid);
    }
  });

  // Recompute when the date range or environment changes, but NOT merely
  // because the app changed — the effect above already recomputes on an app
  // switch (via loadEvents). This is the one page (of 24) that tracks
  // `currentEnvId` directly instead of `scopeKey`: `scopeKey` folds the app
  // id in too, so tracking it here would also re-fire this effect on an app
  // switch — the exact thing `untrack()` below exists to prevent. That used
  // to be masked rather than avoided: it was a no-op only because Svelte 5
  // happens to run effects in registration order and the sibling
  // `loadEvents` effect's first statement is a synchronous `steps = []`
  // before its first `await`, so by the time this effect's untracked
  // `steps.length >= 2` check ran on an app switch, it always saw `[]`.
  // Neither ordering nor that reset is a documented contract, and without
  // one, `compute()` has no request-ordering guard — a stale-steps request
  // for the old app could resolve after a fresh one and overwrite a correct
  // funnel with one computed from the old step list. Tracking `currentEnvId`
  // directly is the only new trigger this effect actually needs, and it
  // removes the dependency on that ordering instead of relying on it.
  $effect(() => {
    const win = range;
    sessionStore.currentEnvId;
    untrack(() => {
      const aid = sessionStore.currentAppId;
      if (aid && steps.length >= 2) void compute(aid, win);
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
    if (aid) void compute(aid, range);
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

  // Wraps this page's OWN refresh rather than replacing it: the body below
  // does page-specific work a generic sweep would drop. `pageRefresher` adds
  // the server force window and the wait for the recompute to land, so a page
  // button and the one in the top bar now mean the same thing.
  const refresher = pageRefresher(refresh);

  const overallConv = $derived(result ? (result.steps.at(-1)?.conv_from_start ?? 0) : 0);
  const pickerOptions = $derived(available.map((ev) => ({ value: ev.name, label: ev.name })));
</script>

  <PageHeader title={t('funnels.title')} subtitle={t('funnels.subtitle')}>
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || refreshing} />
    {/snippet}
  </PageHeader>

  <div class="page">
    <div class="filters">
      <DateRange
        value={range}
        onchange={(v) => {
          range = v;
          rangeStore.set(v);
        }}
      />
    </div>

  {#if loadingEvents}
    <Skeleton rows={4} />
  {:else if error && available.length === 0}
    <ErrorState title={t('funnels.error.events')} error={catalogView.error ? catalogView : error} onretry={retry} />
  {:else if available.length === 0}
    <!-- The catalogue is a fixed 90 days, not the picker's window — so this is
         the one empty state on the page that may say "yet". -->
    <EmptyState
      title={t('funnels.empty.title')}
      description={t('funnels.empty.body')}
      icon="chart-column"
    />
  {:else}
    {#if saved.length > 0}
      <Card title={t('funnels.card.saved')}>
        <div class="saved-search">
          <SearchInput bind:value={funnelSearch} placeholder={t('funnels.search')} width="280px" />
        </div>
        {#if filteredFunnels.length === 0}
          <EmptyState
            size="section"
            title={t('common.empty.match', { things: t('funnels.noun') })}
            description={t('common.empty.matchHint')}
          />
        {:else}
          <ul class="saved-list">
            {#each filteredFunnels as f (f.id)}
              <li class="saved-item" class:active={f.id === loadedId}>
                <button class="load" type="button" onclick={() => loadFunnel(f)} title={t('funnels.loadThis')}>
                  <span class="sf-name truncate">{f.name}</span>
                  <span class="sf-meta">{t('funnels.stepCount', { n: formatNumber(f.steps.length) })}{#if f.created_by_name} · {f.created_by_name}{/if}</span>
                </button>
                <div class="sf-actions">
                  <button
                    type="button"
                    title={t('funnels.duplicate')}
                    aria-label={t('funnels.duplicate')}
                    use:lockTip={writeLock}
                    onclick={() => duplicateFunnel(f)}
                  >
                    <Icon name={writeLock ? 'lock' : 'copy'} size={14} />
                  </button>
                  <button
                    type="button"
                    title={t('common.delete')}
                    aria-label={t('common.delete')}
                    use:lockTip={writeLock}
                    onclick={() => openDeleteConfirm(f)}
                  >
                    <Icon name={writeLock ? 'lock' : 'x'} size={14} />
                  </button>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
    {/if}

    <div class="grid">
      <Card title={t('funnels.card.builder')}>
        {#if steps.length === 0}
          <EmptyState size="section" title={t('funnels.empty.noSteps')} description={t('funnels.addTwoSteps')} />
        {:else}
          <ol class="steps">
            {#each steps as step, i (i)}
              <li class="step">
                <span class="snum">{i + 1}</span>
                <span class="sname mono truncate">{step}</span>
                <button class="remove" type="button" title={t('funnels.removeStep')} aria-label={t('funnels.removeStep')} onclick={() => removeStep(i)}><Icon name="x" size={14} /></button>
              </li>
            {/each}
          </ol>
        {/if}

        <div class="add-row">
          <Select bind:value={picked} options={pickerOptions} ariaLabel={t('funnels.eventToAdd')} size="sm" />
          <Button variant="secondary" size="sm" onclick={addStep}>{t('funnels.addStep')}</Button>
        </div>

        <div class="compute-row">
          <Button variant="primary" onclick={onCompute} disabled={steps.length < 2} loading={computing}>
            {t('funnels.compute')}
          </Button>
          {#if loadedId}
            <Button variant="secondary" size="sm" onclick={openUpdateDialog} disabled={steps.length < 2} lockedReason={writeLock}>{t('funnels.update')}</Button>
            <Button variant="secondary" size="sm" onclick={openSaveDialog} disabled={steps.length < 2} lockedReason={writeLock}>{t('funnels.saveAsNew')}</Button>
          {:else}
            <Button variant="secondary" size="sm" onclick={openSaveDialog} disabled={steps.length < 2} lockedReason={writeLock}>{t('funnels.saveTemplate')}</Button>
          {/if}
          {#if steps.length < 2}
            <span class="faint hint">{t('funnels.needTwoSteps')}</span>
          {/if}
        </div>
      </Card>

      <Card title={t('funnels.card.results')}>
        {#snippet actions()}
          {#if computing && result}
            <span class="updating-tag"><Spinner size={13} /> {t('funnels.updating')}</span>
          {/if}
        {/snippet}
        {#if computing && !result}
          <Skeleton rows={6} />
        {:else if error && !result}
          <ErrorState title={t('funnels.error.compute')} error={computeFailure ?? error} onretry={onCompute} size="section" />
        {:else if result}
          <div class="summary" class:updating={computing}>
            <StatTiles min={150}>
              <StatTile label={t('funnels.entered')} value={formatNumber(result.total_entered)} />
              <StatTile label={t('funnels.overallConversion')} value={formatPercent(overallConv)} />
            </StatTiles>
          </div>
          <div class="chart-wrap">
            <FunnelChart result={result} />
          </div>
        {:else}
          <EmptyState size="section" title={t('funnels.empty.notComputed')} description={t('funnels.computePrompt')} />
        {/if}
      </Card>
    </div>
  {/if}
  </div>

  <Modal
    bind:open={showDetailsDialog}
    title={dialogMode === 'edit' ? t('funnels.dialog.editTitle') : t('funnels.dialog.saveTitle')}
    onclose={closeDialog}
  >
    <div class="dialog-form">
      <Input label={t('common.name')} bind:value={dialogName} placeholder={t('funnels.placeholder.name')} required />
      <Textarea
        id="funnel-desc"
        label={t('funnels.description')}
        labelNote={t('funnels.optional')}
        bind:value={dialogDesc}
        placeholder={t('funnels.placeholder.description')}
      />
      <p class="dialog-hint">
        {t('funnels.dialog.stepsHint', { n: formatNumber(steps.length) })}
      </p>
    </div>
    {#snippet footer()}
      <Button variant="secondary" onclick={closeDialog} disabled={savingDialog}>{t('common.cancel')}</Button>
      <Button
        variant="primary"
        onclick={submitDialog}
        disabled={!dialogName.trim() || steps.length < 2}
        loading={savingDialog}
      >
        {dialogMode === 'edit' ? t('funnels.dialog.saveChanges') : t('funnels.saveTemplate')}
      </Button>
    {/snippet}
  </Modal>

  <ConfirmDialog
    bind:open={showDeleteConfirm}
    title={t('funnels.deleteTitle')}
    message={pendingDelete ? t('funnels.deleteMessage', { name: pendingDelete.name }) : ''}
    confirmLabel={t('common.delete')}
    danger
    loading={deleting}
    onconfirm={confirmDelete}
    oncancel={cancelDelete}
  />

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }
  .filters {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .grid {
    display: grid;
    grid-template-columns: minmax(300px, 360px) 1fr;
    gap: var(--page-gap) 28px;
    align-items: start;
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
    background: var(--surface-3);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-strong);
    flex-shrink: 0;
  }
  .sname {
    flex: 1;
    min-width: 0;
    font-size: var(--text-xs);
  }
  .remove {
    background: none;
    border: none;
    color: var(--text-faint);
    font-size: var(--text-xs);
    line-height: 1;
    padding: 3px 5px;
    border-radius: var(--radius-sm);
    flex-shrink: 0;
  }
  .remove:hover {
    color: var(--error);
    background: var(--error-soft);
  }

  /* A grid, not a flex row, so the Select's root stretches to the free
     width without reaching into its markup. */
  .add-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 8px;
    margin-top: 12px;
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
    font-size: var(--text-xs);
  }

  .summary {
    margin-bottom: 20px;
    transition: opacity 0.12s ease;
  }
  .summary.updating {
    opacity: 0.75;
  }
  .updating-tag {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--text-muted);
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
    text-align: start;
  }
  .sf-name {
    font-size: var(--text-sm);
    color: var(--text);
    font-weight: var(--weight-medium);
    max-width: 220px;
  }
  .sf-meta {
    font-size: var(--text-xs);
    color: var(--text-faint);
  }
  .sf-actions {
    display: flex;
    gap: 2px;
    padding-inline-end: 6px;
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
  .dialog-hint {
    font-size: var(--text-xs);
    color: var(--text-faint);
    margin: 0;
  }
</style>
