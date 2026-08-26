<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import {
    purgeApi,
    blockedByEnvFilter,
    isActive,
    totalCount,
    type PurgeCatalog,
    type PurgeJob,
    type PurgeKind,
  } from '../lib/api/purge';

  let catalog = $state<PurgeCatalog | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

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

  async function load() {
    loading = true;
    error = null;
    try {
      const { data } = await purgeApi.catalog();
      catalog = data;
    } catch (e) {
      error = describe(e);
    } finally {
      loading = false;
    }
  }

  function describe(e: unknown): string {
    const anyE = e as { response?: { data?: { error?: string; message?: string } } };
    return (
      anyE?.response?.data?.error ??
      anyE?.response?.data?.message ??
      (e instanceof Error ? e.message : 'Request failed')
    );
  }

  async function startPreview() {
    if (!app || !canPreview) return;
    busy = true;
    error = null;
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
      error = describe(e);
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
        error = describe(e);
      }
    }, 1500);
  }

  async function doConfirm() {
    if (!job || !slugMatches) return;
    busy = true;
    error = null;
    try {
      const { data } = await purgeApi.confirm(job.id, confirmText.trim());
      job = data;
      poll();
    } catch (e) {
      error = describe(e);
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
      error = describe(e);
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
    return k.slug.replace(/_/g, ' ');
  }

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
  <div class="head">
    <div>
      <h1>{t('prose.purge.title')}</h1>
      <p class="sub">
        {t('prose.purge.lede')} <strong>{t('purge.noUndo')}</strong>
      </p>
    </div>
    <Button variant="ghost" onclick={reset} disabled={busy}>
      <Icon name="refresh" /> {t('common.reset')}
    </Button>
  </div>

  <div class="content">
    {#if error}
      <Card><p class="err">{error}</p></Card>
    {/if}

    {#if loading}
      <Card><Skeleton rows={5} /></Card>
    {:else if !app}
      <EmptyState title={t('purge.selectApp')} description={t('purge.selectAppBody')} icon="package" />
    {:else if !job}
      <!-- ================= scope form ================= -->
      <Card>
      <h2>{t('prose.purge.step1')}</h2>
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
            <Badge tone={k.class === 'raw' ? 'neutral' : 'info'}>{k.class}</Badge>
            {#if isBlocked}
              <span class="why">
                {t('prose.purge.blockedKind')}
              </span>
            {/if}
          </label>
        {/each}
      </div>
    </Card>

    <Card>
      <h2>2 — Environments</h2>
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
          <p class="hint warn">{t('purge.selectEnvironment')}</p>
        {/if}
      {/if}
    </Card>

    <Card>
      <h2>{t('prose.purge.step3')}</h2>
      <label class="row">
        <input type="checkbox" bind:checked={allTime} />
        <span><strong>{t('purge.allTime')}</strong> {t('prose.purge.allTimeNote')}</span>
      </label>
      {#if !allTime}
        <div class="range">
          <label>{t('ui.time.from')} <input type="datetime-local" bind:value={rangeStart} /></label>
          <label>To <input type="datetime-local" bind:value={rangeEnd} /></label>
        </div>
        {#if rangeStart && rangeEnd && rangeStart >= rangeEnd}
          <p class="hint warn">{t('purge.badRange')}</p>
        {/if}
      {:else}
        <p class="hint warn">
          {t('purge.unbounded')}
        </p>
      {/if}
    </Card>

    <div class="actions">
      <Button onclick={startPreview} disabled={!canPreview || busy}>
        {busy ? 'Counting…' : 'Preview'}
      </Button>
      <span class="hint">{t('prose.purge.previewFirst')}</span>
    </div>
  {:else}
    <!-- ================= the job ================= -->
    <Card>
      <div class="jobhead">
        <h2>{job.app_name}</h2>
        <Badge tone={statusTone(job.status)}>{job.status}</Badge>
        {#if job.phase !== 'idle' && job.phase !== 'finished'}
          <Badge tone="info">{job.phase}</Badge>
        {/if}
      </div>

      {#if job.status === 'previewing'}
        <p><Spinner /> {t('purge.counting')}</p>
      {:else}
        <table class="counts">
          <thead>
            <tr><th>{t('sourcemaps.column.kind')}</th><th>{t('prose.purge.matched')}</th><th>{t('purge.deleted')}</th></tr>
          </thead>
          <tbody>
            {#each job.kinds as k (k)}
              <tr>
                <td>{k.replace(/_/g, ' ')}</td>
                <td class="n">{formatNumber(job.estimated_counts?.[k] ?? 0)}</td>
                <td class="n">{formatNumber(job.deleted_counts?.[k] ?? 0)}</td>
              </tr>
            {/each}
          </tbody>
          <tfoot>
            <tr>
              <th>{t('common.total')}</th>
              <th class="n">{formatNumber(totalCount(job.estimated_counts))}</th>
              <th class="n">{formatNumber(totalCount(job.deleted_counts))}</th>
            </tr>
          </tfoot>
        </table>

        {#if job.cold_rows_skipped > 0}
          <p class="cold">
            <Icon name="layers" />
            <strong>{formatNumber(job.cold_rows_skipped)} rows will survive.</strong>
            They have already rotated to cold storage
            {#if job.cold_boundary_at}
              (anything before <TimeValue value={job.cold_boundary_at} />)
            {/if}
            and cannot be deleted. Counters are still recomputed against them, so the numbers
            stay truthful — but this data stays in your charts.
          </p>
        {/if}

        {#if job.ingest_active}
          <p class="hint warn">
            {t('prose.purge.drift')}
          </p>
        {/if}

        {#if job.status === 'running' || job.status === 'cancelling'}
          <p class="hint">
            Recomputed {formatNumber(job.rollups_recomputed)} rollups, removed
            {formatNumber(job.rollups_deleted)} with nothing left.
          </p>
        {/if}

        {#if job.error}
          <p class="err">{job.error}</p>
        {/if}
      {/if}

      {#if job.status === 'previewed'}
        <div class="confirm">
          <p>
            {t('purge.confirmSlug')} <code>{job.app_slug}</code>. This deletes the rows
            above and cannot be undone.
          </p>
          <input type="text" bind:value={confirmText} placeholder={job.app_slug} />
          <Button variant="primary" onclick={doConfirm} disabled={!slugMatches || busy}>
            Purge {formatNumber(totalCount(job.estimated_counts))} rows
          </Button>
        </div>
      {/if}

      {#if isActive(job) && job.status !== 'previewing'}
        <Button variant="ghost" onclick={doCancel} disabled={busy}>
          {t('purge.cancelNote')}
        </Button>
      {/if}

      {#if !isActive(job)}
        <Button variant="ghost" onclick={reset}>{t('purge.startAnother')}</Button>
      {/if}
    </Card>
  {/if}

  {#if catalog?.jobs?.length}
    <Card>
      <h2>{t('alerts.tab.history')}</h2>
      <DataTable>
        {#snippet head()}
          <tr>
            <th>{t('nav.selectApp')}</th>
            <th>{t('common.status')}</th>
            <th class="n">{t('prose.purge.kinds')}</th>
            <th class="n">{t('purge.deleted')}</th>
            <th>{t('prose.purge.requestedBy')}</th>
            <th>{t('ui.opModal.when')}</th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each catalog?.jobs ?? [] as j (j.id)}
            <tr>
              <td>{j.app_name}</td>
              <td><Badge tone={statusTone(j.status)}>{j.status}</Badge></td>
              <td class="n">{(j.kinds ?? []).length}</td>
              <td class="n">{formatNumber(totalCount(j.deleted_counts))}</td>
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
    gap: 16px;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 1rem;
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
  }
  h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  .sub,
  .hint {
    color: var(--text-muted);
    font-size: 0.875rem;
    margin: 0.25rem 0;
  }
  .hint.warn {
    color: var(--warning, #b45309);
  }
  .err {
    color: var(--danger, #b91c1c);
  }
  .kinds {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 0.5rem;
  }
  .kind,
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.35rem 0;
  }
  .kind.blocked {
    opacity: 0.55;
  }
  .kind .why {
    font-size: 0.75rem;
    color: var(--text-muted);
    flex-basis: 100%;
  }
  .name {
    text-transform: capitalize;
  }
  .range {
    display: flex;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .range label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.875rem;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 1rem;
    margin: 1rem 0;
    flex-wrap: wrap;
  }
  .jobhead {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }
  .counts {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.875rem;
  }
  .counts th,
  .counts td {
    text-align: start;
    padding: 0.35rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  .counts .n {
    text-align: end;
    font-variant-numeric: tabular-nums;
  }
  .cold {
    margin-top: 0.75rem;
    padding: 0.6rem 0.75rem;
    border-radius: 6px;
    background: var(--surface-2, rgba(127, 127, 127, 0.08));
    font-size: 0.875rem;
  }
  .confirm {
    margin-top: 1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    align-items: flex-start;
  }
  .confirm code {
    padding: 0.1rem 0.35rem;
    border-radius: 4px;
    background: var(--surface-2, rgba(127, 127, 127, 0.12));
  }
</style>
