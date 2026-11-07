<script lang="ts">
  import { loadErrorText } from '../lib/models/load-error';
  import { errorMessage } from '../lib/api/client';
  import { purgeKindName } from '../lib/models/purge-kinds';
  import { formatNumber, labelFor, t, type MessageKey } from '../lib/i18n';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import {
    purgeApi,
    blockedByEnvFilter,
    isActive,
    totalCount,
    type PurgeCatalog,
    type PurgeJob,
    type PurgeKind,
    type PurgeStatus,
  } from '../lib/api/purge';

  // Cached view (lib/stores/cached-view.svelte.ts): the catalogue paints
  // instantly on return instead of blanking while the same request runs again.
  const view = new CachedView<PurgeCatalog>();

  // `reload()` replays the view's most recent key and fetcher with force,
  // which is exactly what Refresh means here — the page's loads are driven by
  // effects, so there is no `load(force)` to call and reconstructing the key
  // by hand would be a second definition of it, free to drift.
  const refresher = pageRefresher(async () => {
    await view.reload();
  });

  const catalog = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  /**
   * Two error sources, deliberately not merged into one variable.
   *
   * `view.error` is the catalogue read failing; `actionError` is a preview,
   * confirm or cancel failing. They have different lifetimes — an action error
   * must survive a background revalidate, and a load error must not be cleared
   * by starting an unrelated action — so the page keeps both and renders
   * whichever is live, the action first because it is what the user just did.
   */
  const revalidating = $derived(view.revalidating);
  let actionError = $state<string | null>(null);

  // --- the form ------------------------------------------------------------
  let selectedKinds = $state<Set<string>>(new Set());
  let envFilterActive = $state(false);
  let selectedEnvs = $state<Set<string>>(new Set());
  let allTime = $state(false);
  let rangeStart = $state('');
  let rangeEnd = $state('');

  // --- the job in flight ---------------------------------------------------
  let job = $state<PurgeJob | null>(null);
  let confirmText = $state('');
  let busy = $state(false);
  let pollTimer: ReturnType<typeof setTimeout> | null = null;

  const app = $derived(sessionStore.currentApp);
  const environments = $derived(sessionStore.environments);

  /**
   * Kinds the API will refuse while an environment filter is active.
   *
   * `devices`, `issues`, `persons` and `inspector` have no `environment_id`
   * column, so no row can be attributed to one environment. Disabled rather
   * than silently narrowed — accepting the tick and doing something other than
   * what it promises is the worse outcome, and the server refuses it anyway.
   */
  const blocked = $derived(blockedByEnvFilter(catalog?.kinds ?? [], envFilterActive));

  // A blocked kind must not stay ticked when the filter is switched on, or the
  // request carries a selection the UI is simultaneously showing as disabled.
  $effect(() => {
    if (blocked.size === 0) return;
    const next = new Set([...selectedKinds].filter((k) => !blocked.has(k)));
    if (next.size !== selectedKinds.size) selectedKinds = next;
  });

  const canPreview = $derived(
    !!app &&
      selectedKinds.size > 0 &&
      (allTime || (!!rangeStart && !!rangeEnd && rangeStart < rangeEnd)) &&
      (!envFilterActive || selectedEnvs.size > 0),
  );

  const slugMatches = $derived(!!job && confirmText.trim() === job.app_slug);

  async function load(force = false) {
    // `scopeKey` carries the selected environment, which the axios interceptor
    // adds to the request but which appears in no argument here. Omit it and
    // one environment's catalogue keys the same as another's.
    await view.load(
      viewKey('purge.catalog', sessionStore.scopeKey),
      async () => (await purgeApi.catalog()).data,
      force,
    );
  }

  async function startPreview() {
    if (!app || !canPreview) return;
    busy = true;
    actionError = null;
    confirmText = '';
    try {
      const { data } = await purgeApi.preview({
        app_id: app.id,
        kinds: [...selectedKinds],
        // Omitted entirely for "every environment". An empty array is a scope
        // that matches nothing and the API refuses it — the two must never be
        // spelled the same way.
        ...(envFilterActive ? { environment_ids: [...selectedEnvs] } : {}),
        ...(allTime
          ? { all_time: true }
          : {
              range_start: new Date(rangeStart).toISOString(),
              range_end: new Date(rangeEnd).toISOString(),
            }),
      });
      job = data;
      poll();
    } catch (e) {
      actionError = errorMessage(e, t('purge.requestFailed'));
    } finally {
      busy = false;
    }
  }

  /**
   * Poll while the job is moving.
   *
   * Counting and executing both happen in `sauron-tier`, so the only way the
   * UI learns the outcome is by asking. The timer is cleared on every new
   * schedule so a re-entry cannot leave two chains running.
   */
  function poll() {
    if (pollTimer) clearTimeout(pollTimer);
    if (!job || !isActive(job)) return;
    pollTimer = setTimeout(async () => {
      if (!job) return;
      try {
        const { data } = await purgeApi.get(job.id);
        job = data;
        poll();
      } catch (e) {
        actionError = errorMessage(e, t('purge.requestFailed'));
      }
    }, 1500);
  }

  async function doConfirm() {
    if (!job || !slugMatches) return;
    busy = true;
    actionError = null;
    try {
      const { data } = await purgeApi.confirm(job.id, confirmText.trim());
      job = data;
      poll();
    } catch (e) {
      actionError = errorMessage(e, t('purge.requestFailed'));
    } finally {
      busy = false;
    }
  }

  async function doCancel() {
    if (!job) return;
    busy = true;
    try {
      const { data } = await purgeApi.cancel(job.id);
      job = data;
      poll();
    } catch (e) {
      actionError = errorMessage(e, t('purge.requestFailed'));
    } finally {
      busy = false;
    }
  }

  function reset() {
    if (pollTimer) clearTimeout(pollTimer);
    job = null;
    confirmText = '';
    load();
  }

  function toggle(set: Set<string>, key: string): Set<string> {
    const next = new Set(set);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    return next;
  }

  function kindLabel(k: PurgeKind): string {
    return purgeKindName(k.slug);
  }

  const STATUS_LABELS: Record<PurgeStatus, MessageKey> = {
    previewing: 'purge.status.previewing',
    previewed: 'purge.status.previewed',
    pending: 'purge.status.pending',
    running: 'purge.status.running',
    cancelling: 'purge.status.cancelling',
    done: 'purge.status.done',
    failed: 'purge.status.failed',
    cancelled: 'purge.status.cancelled',
  };
  const PHASE_LABELS: Record<PurgeJob['phase'], MessageKey> = {
    idle: 'purge.phase.idle',
    counting: 'purge.phase.counting',
    delete: 'purge.phase.delete',
    recompute: 'purge.phase.recompute',
    finished: 'purge.phase.finished',
  };
  const statusLabel = (s: PurgeStatus): string => labelFor(STATUS_LABELS, s);
  const phaseLabel = (p: PurgeJob['phase']): string => labelFor(PHASE_LABELS, p);
  const classLabel = (c: PurgeKind['class']): string =>
    c === 'raw' ? t('purge.class.raw') : c === 'rollup' ? t('purge.class.rollup') : c;

  function statusTone(s: string): 'success' | 'error' | 'warning' | 'neutral' {
    if (s === 'done') return 'success';
    if (s === 'failed') return 'error';
    if (s === 'cancelled' || s === 'cancelling') return 'warning';
    return 'neutral';
  }

  $effect(() => {
    load();
    return () => {
      if (pollTimer) clearTimeout(pollTimer);
    };
  });

  /**
   * Reset the whole form when the app or environment scope changes.
   *
   * Keyed on `sessionStore.scopeKey` rather than `currentAppId`, so an
   * environment switch counts too. This is not merely a refresh: the selected
   * environment ids are ENROLLMENT ids belonging to the app that was current
   * when they were ticked. Carrying them across an app switch would build a
   * request whose environments are not enrolled in the new app — rejected by
   * the API if you are lucky, and scoped to nothing if the validation ever
   * loosened. Dropping a half-built destructive scope on a context switch is
   * also just the right default.
   *
   * A job already in flight is deliberately NOT cleared: it belongs to the app
   * it was created against, is identified by id, and the operator needs to see
   * how it ends.
   */
  $effect(() => {
    const key = sessionStore.scopeKey;
    void key;
    if (job) return;
    selectedKinds = new Set();
    selectedEnvs = new Set();
    envFilterActive = false;
  });
</script>

<AdminShell>
  <PageHeader
    title={t('prose.purge.title')}
    subtitle={`${t('prose.purge.lede')} ${t('purge.noUndo')}`}
  >
    {#snippet freshness()}
      <Freshness fetchedAt={view.fetchedAt} {revalidating} />
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || revalidating} />
    {/snippet}
    {#snippet actions()}
      <Button variant="ghost" onclick={reset} disabled={busy}>
        <Icon name="refresh" /> {t('common.reset')}
      </Button>
    {/snippet}
  </PageHeader>

  <div class="content">
    {#if actionError}
      <InlineNotice tone="error">{actionError}</InlineNotice>
    {/if}
    {#if view.error && view.data !== undefined}
      <!-- A forced Refresh failed over the catalogue we already had: it stays. -->
      <InlineNotice tone="warning">
        {t('ui.staleResults', { error: loadErrorText(view) })}
        {#snippet action()}
          <Button variant="ghost" size="sm" onclick={refresher.run}>{t('common.retry')}</Button>
        {/snippet}
      </InlineNotice>
    {/if}

    {#if loading}
      <Skeleton rows={5} />
    {:else if view.error && view.data === undefined}
      <ErrorState
        title={t('common.couldntLoad', { thing: t('purge.thing') })}
        error={view}
        onretry={refresher.run}
      />
    {:else if !app}
      <EmptyState title={t('purge.selectApp')} description={t('purge.selectAppBody')} icon="package" />
    {:else if !job}
      <!-- ================= scope form ================= -->
      <Card title={t('prose.purge.step1')}>
        <p class="hint">
          {t('prose.purge.rollups')}
        </p>
        <div class="kinds">
          {#each catalog?.kinds ?? [] as k (k.slug)}
            {@const isBlocked = blocked.has(k.slug)}
            <label class="kind" class:blocked={isBlocked}>
              <input
                type="checkbox"
                disabled={isBlocked}
                checked={selectedKinds.has(k.slug)}
                onchange={() => (selectedKinds = toggle(selectedKinds, k.slug))}
              />
              <span class="name">{kindLabel(k)}</span>
              <Badge tone={k.class === 'raw' ? 'neutral' : 'info'} size="sm">{classLabel(k.class)}</Badge>
              {#if isBlocked}
                <span class="why">
                  {t('prose.purge.blockedKind')}
                </span>
              {/if}
            </label>
          {/each}
        </div>
      </Card>

      <Card title={t('purge.step2')}>
        <label class="row">
          <input type="checkbox" bind:checked={envFilterActive} />
          <span>{t('prose.purge.limitEnvs')}</span>
        </label>
        {#if !envFilterActive}
          <p class="hint">
            {t('purge.allEnvironments')}
          </p>
        {:else}
          <div class="envs">
            {#each environments as e (e.id)}
              <label class="row">
                <input
                  type="checkbox"
                  checked={selectedEnvs.has(e.id)}
                  onchange={() => (selectedEnvs = toggle(selectedEnvs, e.id))}
                />
                <span>{e.name}</span>
              </label>
            {/each}
          </div>
          {#if selectedEnvs.size === 0}
            <div class="note"><InlineNotice tone="warning">{t('purge.selectEnvironment')}</InlineNotice></div>
          {/if}
        {/if}
      </Card>

      <Card title={t('prose.purge.step3')}>
        <label class="row">
          <input type="checkbox" bind:checked={allTime} />
          <span><strong>{t('purge.allTime')}</strong> {t('prose.purge.allTimeNote')}</span>
        </label>
        {#if !allTime}
          <div class="range">
            <Input label={t('ui.time.from')} type="datetime-local" bind:value={rangeStart} />
            <Input label={t('purge.to')} type="datetime-local" bind:value={rangeEnd} />
          </div>
          {#if rangeStart && rangeEnd && rangeStart >= rangeEnd}
            <div class="note"><InlineNotice tone="warning">{t('purge.badRange')}</InlineNotice></div>
          {/if}
        {:else}
          <div class="note">
            <InlineNotice tone="warning">{t('purge.unbounded')}</InlineNotice>
          </div>
        {/if}
      </Card>

      <div class="actions">
        <Button variant="primary" onclick={startPreview} loading={busy} disabled={!canPreview}>
          {t('purge.preview')}
        </Button>
        <span class="hint">{t('prose.purge.previewFirst')}</span>
      </div>
    {:else}
      <!-- ================= the job ================= -->
      <Card title={job.app_name}>
        {#snippet actions()}
          <Badge tone={statusTone(job!.status)} size="sm">{statusLabel(job!.status)}</Badge>
          {#if job!.phase !== 'idle' && job!.phase !== 'finished'}
            <Badge tone="info" size="sm">{phaseLabel(job!.phase)}</Badge>
          {/if}
        {/snippet}

        {#if job.status === 'previewing'}
          <p class="counting"><Spinner /> {t('purge.counting')}</p>
        {:else}
          <DataTable>
            {#snippet head()}
              <tr>
                <th>{t('purge.column.kind')}</th>
                <th class="num">{t('prose.purge.matched')}</th>
                <th class="num">{t('purge.deleted')}</th>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each job!.kinds as k (k)}
                <tr>
                  <td class="name">{purgeKindName(k)}</td>
                  <td class="num">{formatNumber(job!.estimated_counts?.[k] ?? 0)}</td>
                  <td class="num">{formatNumber(job!.deleted_counts?.[k] ?? 0)}</td>
                </tr>
              {/each}
              <tr class="total">
                <td>{t('common.total')}</td>
                <td class="num">{formatNumber(totalCount(job!.estimated_counts))}</td>
                <td class="num">{formatNumber(totalCount(job!.deleted_counts))}</td>
              </tr>
            {/snippet}
          </DataTable>

          {#if job.cold_rows_skipped > 0}
            <div class="note">
              <InlineNotice tone="info">
                <strong>{t('purge.coldSurvive', { n: formatNumber(job.cold_rows_skipped) })}</strong>
                {t('purge.coldRotated')}
                {#if job.cold_boundary_at}
                  ({t('purge.coldBefore')} <TimeValue value={job.cold_boundary_at} />)
                {/if}
                {t('purge.coldStays')}
              </InlineNotice>
            </div>
          {/if}

          {#if job.ingest_active}
            <div class="note">
              <InlineNotice tone="warning">{t('prose.purge.drift')}</InlineNotice>
            </div>
          {/if}

          {#if job.status === 'running' || job.status === 'cancelling'}
            <p class="hint">
              {t('purge.rollupProgress', {
                recomputed: formatNumber(job.rollups_recomputed),
                removed: formatNumber(job.rollups_deleted),
              })}
            </p>
          {/if}

          {#if job.error}
            <div class="note"><InlineNotice tone="error">{job.error}</InlineNotice></div>
          {/if}
        {/if}

        {#if job.status === 'previewed'}
          <!-- Bulk destruction keeps type-to-confirm: the slug IS the
               confirmation, so this is not a ConfirmDialog. -->
          <div class="confirm">
            <p>
              {t('purge.confirmSlug')} <code>{job.app_slug}</code>. {t('purge.confirmBody')}
            </p>
            <div class="confirm-row">
              <div class="slug-field">
                <Input label={t('purge.confirmLabel')} bind:value={confirmText} placeholder={job.app_slug} />
              </div>
              <Button variant="danger" onclick={doConfirm} disabled={!slugMatches || busy}>
                {t('purge.purgeRows', { n: formatNumber(totalCount(job.estimated_counts)) })}
              </Button>
            </div>
          </div>
        {/if}

        {#if isActive(job) && job.status !== 'previewing'}
          <div class="job-actions">
            <Button variant="secondary" onclick={doCancel} disabled={busy}>
              {t('purge.cancelNote')}
            </Button>
          </div>
        {/if}

        {#if !isActive(job)}
          <div class="job-actions">
            <Button variant="secondary" onclick={reset}>{t('purge.startAnother')}</Button>
          </div>
        {/if}
      </Card>
    {/if}

    {#if catalog?.jobs?.length}
      <Card title={t('alerts.tab.history')}>
        <DataTable>
          {#snippet head()}
            <tr>
              <th>{t('nav.selectApp')}</th>
              <th>{t('common.status')}</th>
              <th class="num">{t('prose.purge.kinds')}</th>
              <th class="num">{t('purge.deleted')}</th>
              <th>{t('prose.purge.requestedBy')}</th>
              <th>{t('ui.opModal.when')}</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each catalog?.jobs ?? [] as j (j.id)}
              <tr>
                <td>{j.app_name}</td>
                <td><Badge tone={statusTone(j.status)} size="sm">{statusLabel(j.status)}</Badge></td>
                <td class="num">{formatNumber((j.kinds ?? []).length)}</td>
                <td class="num">{formatNumber(totalCount(j.deleted_counts))}</td>
                <td>{j.requested_by_email}</td>
                <td><TimeValue value={j.requested_at} /></td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>
      </Card>
    {/if}
  </div>
</AdminShell>

<style>
  .content {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 4px 0;
  }
  .note {
    margin-top: 10px;
  }
  .kinds {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 8px;
    margin-top: 8px;
  }
  .kind,
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 0;
  }
  .kind {
    flex-wrap: wrap;
  }
  .kind.blocked {
    opacity: 0.55;
  }
  .kind .why {
    font-size: var(--text-xs);
    color: var(--text-muted);
    flex-basis: 100%;
  }
  .range {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
    margin-top: 8px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
  }
  .counting {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
  }
  .total td {
    font-weight: var(--weight-medium);
  }
  .confirm {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: var(--text-sm);
  }
  .confirm code {
    padding: 1px 6px;
    border-radius: var(--radius-sm);
    background: var(--surface-3);
  }
  /* Bottom-aligned: the field now carries a label above its box, and the
     button belongs level with the box, not with the label. */
  .confirm-row {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    flex-wrap: wrap;
  }
  .slug-field {
    width: 260px;
    max-width: 100%;
  }
  .job-actions {
    margin-top: 12px;
  }
</style>
