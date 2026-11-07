<script lang="ts">
  import { loadErrorText } from '../lib/models/load-error';
  import { errorMessage } from '../lib/api/client';
  import { labelFor, t, type MessageKey } from '../lib/i18n';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import Select from '../lib/components/ui/Select.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import FactList from '../lib/components/FactList.svelte';
  import Fact from '../lib/components/Fact.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import {
    listIngestFailures,
    getIngestFailurePayloads,
    retryIngestFailure,
    dropIngestFailure,
    type IngestFailure,
    type IngestFailurePage,
    type IngestFailurePayload,
  } from '../lib/api/ingest-failures';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import {
    describeKind,
    describeRecovery,
    fmt,
    shortFingerprint,
    shortMessage,
    statusTone,
    wasAutoRetried,
  } from '../lib/models/ingest-failures';

  let statusFilter = $state<string>('failed');

  // Cached view (lib/stores/cached-view.svelte.ts): only the FIRST page is
  // cached, keyed on the status filter. Pages walked into via "Load more" are
  // held separately in `appended` and deliberately not cached — their cursors
  // are only meaningful behind the exact first page that produced them, so
  // caching them under this key would let a revisit reassemble a list out of
  // two different snapshots.
  const view = new CachedView<IngestFailurePage>();
  let appended = $state<IngestFailure[]>([]);
  // Cursor for the NEXT "Load more". Null until a page has been appended, at
  // which point it supersedes the first page's own cursor.
  let appendedCursor = $state<string | null>(null);
  let loadingMore = $state(false);

  // `view.data.failures` is the cache's own array; spreading builds a new one so
  // no render path can write back through it.
  const failures = $derived([...(view.data?.failures ?? []), ...appended]);
  const nextCursor = $derived(
    appended.length > 0 ? appendedCursor : (view.data?.next_cursor ?? null),
  );
  const loading = $derived(view.loading);
  const revalidating = $derived(view.revalidating);

  // An action's failure is reported in its own notice; a failed LOAD replaces
  // the list with ErrorState (or, over rows already on screen, says they are
  // stale). Kept apart so a failed Retry never blanks the list.
  let actionError = $state<string | null>(null);

  const statusOptions = $derived([
    { value: 'failed', label: t('failures.state.needsAttention') },
    { value: 'requeued', label: t('failures.state.retrying') },
    { value: 'resolved', label: t('failures.state.resolved') },
    { value: '', label: t('common.all') },
  ]);
  const STATUS_LABELS: Record<IngestFailure['status'], MessageKey> = {
    failed: 'failures.state.needsAttention',
    requeued: 'failures.state.retrying',
    resolved: 'failures.state.resolved',
  };
  const statusLabel = (s: IngestFailure['status']): string => labelFor(STATUS_LABELS, s);

  // Drill-down state.
  let selected = $state<IngestFailure | null>(null);
  let payloads = $state<IngestFailurePayload[]>([]);
  let payloadsLoading = $state(false);

  // Confirmation for the irreversible action.
  let confirmDrop = $state<IngestFailure | null>(null);
  let acting = $state(false);
  let notice = $state<string | null>(null);

  const dropLock = $derived(lockedBy('org:manage'));

  /**
   * `force` bypasses the fresh-window short-circuit — an explicit Refresh, and
   * the re-list after a retry or a drop, both mean "go to the network now".
   *
   * Any load resets the appended pages: they were walked out of a first page
   * that is being replaced, so keeping them would splice rows from the old
   * snapshot onto the new one.
   */
  async function load(force = false) {
    actionError = null;
    appended = [];
    appendedCursor = null;
    await view.load(
      viewKey('ingest-failures.list', statusFilter),
      () => listIngestFailures({ status: statusFilter || undefined, limit: 50 }),
      force,
    );
  }

  /** Refresh must reach the network, so it always forces. */
  async function refresh() {
    viewCache.invalidate('ingest-failures.list');
    await load(true);
  }

  // Wraps this page's OWN refresh rather than replacing it: the body below
  // does page-specific work a generic sweep would drop. `pageRefresher` adds
  // the server force window and the wait for the recompute to land, so a page
  // button and the one in the top bar now mean the same thing.
  const refresher = pageRefresher(refresh);

  async function loadMore() {
    const cursor = nextCursor;
    if (!cursor) return;
    loadingMore = true;
    try {
      const page = await listIngestFailures({
        status: statusFilter || undefined,
        limit: 50,
        cursor,
      });
      appended = [...appended, ...page.failures];
      appendedCursor = page.next_cursor;
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      loadingMore = false;
    }
  }

  async function openDetail(f: IngestFailure) {
    selected = f;
    payloads = [];
    payloadsLoading = true;
    try {
      payloads = await getIngestFailurePayloads(f.id);
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      payloadsLoading = false;
    }
  }

  async function doRetry(f: IngestFailure) {
    acting = true;
    notice = null;
    try {
      const r = await retryIngestFailure(f.id);
      // Reports what actually happened, including the part that did not. A
      // retry that silently omits its failures reads as a success and sends the
      // operator away believing the problem is resolved.
      const parts = [t('failures.notice.requeued', { n: fmt(r.requeued) })];
      if (r.failed > 0) parts.push(t('failures.notice.requeueFailed', { n: fmt(r.failed) }));
      if (r.unrecoverable > 0)
        parts.push(t('failures.notice.unrecoverable', { n: fmt(r.unrecoverable) }));
      notice = parts.join(' ');
      await refresh();
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      acting = false;
    }
  }

  async function doDrop() {
    if (!confirmDrop) return;
    acting = true;
    try {
      await dropIngestFailure(confirmDrop.id);
      notice = t('failures.notice.dropped', { kind: describeKind(confirmDrop.error_kind) });
      confirmDrop = null;
      selected = null;
      await refresh();
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      acting = false;
    }
  }

  $effect(() => {
    // Re-reads when the filter changes. `statusFilter` is the only dependency
    // that should retrigger a load.
    statusFilter;
    load();
  });
</script>

<AdminShell>
  <PageHeader title={t('failures.title')} subtitle={t('prose.failures.lede')}>
    {#snippet freshness()}
      <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || loading || revalidating} />
    {/snippet}
  </PageHeader>

  <div class="page">
    <div class="filters">
      <Select label={t('common.status')} bind:value={statusFilter} options={statusOptions} />
    </div>

    {#if notice}
      <InlineNotice tone="info">{notice}</InlineNotice>
    {/if}
    {#if actionError}
      <InlineNotice tone="error">{actionError}</InlineNotice>
    {/if}
    {#if view.error && view.data !== undefined}
      <!-- A forced Refresh failed over rows we already had: they stay. -->
      <InlineNotice tone="warning">
        {t('ui.staleResults', { error: loadErrorText(view) })}
        {#snippet action()}
          <Button variant="ghost" size="sm" onclick={refresher.run}>{t('common.retry')}</Button>
        {/snippet}
      </InlineNotice>
    {/if}

    {#if loading}
      <Skeleton rows={6} />
    {:else if view.error && view.data === undefined}
      <ErrorState
        title={t('common.couldntLoad', { thing: t('failures.thing') })}
        error={view}
        onretry={refresher.run}
      />
    {:else if failures.length === 0}
      {#if statusFilter === 'failed'}
        <EmptyState
          title={t('failures.empty.title')}
          description={t('failures.empty.body')}
          icon="check"
        />
      {:else}
        <EmptyState
          title={t('common.empty.match', { things: t('failures.things') })}
          description={t('common.empty.matchHint')}
          icon="search"
        />
      {/if}
    {:else}
      <div>
        <DataTable>
          {#snippet head()}
            <tr>
              <th>{t('failures.column.cause')}</th>
              <th>{t('nav.selectApp')}</th>
              <th class="num">{t('failures.column.occurrences')}</th>
              <th class="num">{t('failures.state.recoverable')}</th>
              <th>{t('common.status')}</th>
              <th>{t('explore.column.lastSeen')}</th>
              <th></th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each failures as f (f.id)}
              <tr class="clickable" onclick={() => openDetail(f)}>
                <td>
                  <div class="cause">
                    <strong>{describeKind(f.error_kind)}</strong>
                    <code class="fp">{shortFingerprint(f.fingerprint)}</code>
                  </div>
                  <div class="msg">{shortMessage(f.error_message)}</div>
                </td>
                <td>{f.app_name || '—'}</td>
                <td class="num">{fmt(f.occurrences)}</td>
                <td class="num">
                  {fmt(f.retained)}
                  {#if f.dropped > 0}
                    <!--
                      Shown in the row, not only in the drill-down. A count of
                      retained payloads sitting alone next to a Retry button reads
                      as full coverage; the loss has to be visible at the same
                      glance as the action.
                    -->
                    <div class="loss">{t('failures.lost', { n: fmt(f.dropped) })}</div>
                  {/if}
                </td>
                <td><Badge tone={statusTone(f.status)} size="sm">{statusLabel(f.status)}</Badge></td>
                <td><TimeValue value={f.last_seen_at} /></td>
                <td class="actions">
                  <Button
                    size="sm"
                    variant="secondary"
                    loading={acting}
                    lockedReason={dropLock}
                    onclick={(e) => {
                      e.stopPropagation();
                      doRetry(f);
                    }}
                  >
                    {t('common.retry')}
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    lockedReason={dropLock}
                    onclick={(e) => {
                      e.stopPropagation();
                      confirmDrop = f;
                    }}
                  >
                    {t('failures.drop')}
                  </Button>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        {#if nextCursor}
          <div class="more">
            <Button variant="secondary" loading={loadingMore} onclick={loadMore}>{t('failures.loadMore')}</Button>
          </div>
        {/if}
      </div>
    {/if}
  </div>
</AdminShell>

{#if selected}
  {@const rec = describeRecovery(selected)}
  <Modal open title={describeKind(selected.error_kind)} onclose={() => (selected = null)}>
    <div class="detail">
      <p class="detail-msg">{selected.error_message}</p>

      <div class="facts">
        <FactList>
          <Fact label={t('failures.column.fingerprint')} mono>{selected.fingerprint}</Fact>
          <Fact label={t('nav.selectApp')}>{selected.app_name || t('failures.unknownApp')}</Fact>
          <Fact label={t('explore.column.firstSeen')}><TimeValue value={selected.first_seen_at} /></Fact>
          <Fact label={t('explore.column.lastSeen')}><TimeValue value={selected.last_seen_at} /></Fact>
          <Fact label={t('failures.state.autoRetried')}>
            {wasAutoRetried(selected.error_kind) ? t('failures.autoRetriedYes') : t('failures.autoRetriedNo')}
          </Fact>
        </FactList>
      </div>

      <div class="recovery">
        <InlineNotice tone={rec.level === 'full' ? 'success' : 'error'}>{rec.summary}</InlineNotice>
      </div>

      <h3 class="section-label payloads-label">{t('failures.retainedPayloads')}</h3>
      {#if payloadsLoading}
        <Skeleton rows={3} />
      {:else if payloads.length === 0}
        <p class="muted">{t('failures.noPayloads')}</p>
      {:else}
        {#each payloads as p (p.id)}
          <details>
            <summary>
              <TimeValue value={p.created_at} />
              {#if p.attempts > 0}
                <Badge tone="neutral" size="sm">{t('failures.attempts', { n: fmt(p.attempts) })}</Badge>
              {/if}
            </summary>
            <pre>{JSON.stringify(p.payload, null, 2)}</pre>
          </details>
        {/each}
      {/if}
    </div>
  </Modal>
{/if}

<ConfirmDialog
  open={confirmDrop !== null}
  title={t('failures.confirmDrop')}
  message={confirmDrop
    ? `${
        confirmDrop.retained === 1
          ? t('failures.confirmDropBodyOne', { kind: describeKind(confirmDrop.error_kind) })
          : t('failures.confirmDropBodyMany', {
              n: fmt(confirmDrop.retained),
              kind: describeKind(confirmDrop.error_kind),
            })
      } ${t('prose.failures.auditFirst')}`
    : ''}
  confirmLabel={t('failures.dropPermanently')}
  danger
  loading={acting}
  onconfirm={doDrop}
  oncancel={() => (confirmDrop = null)}
/>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    align-items: flex-end;
  }
  .cause {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fp {
    font-size: var(--text-2xs);
    color: var(--text-faint);
  }
  .msg {
    color: var(--text-muted);
    font-size: var(--text-xs);
    margin-top: 2px;
    max-width: 60ch;
  }
  .loss {
    font-size: var(--text-xs);
    color: var(--error);
    font-weight: var(--weight-medium);
  }
  .actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  .more {
    display: flex;
    justify-content: center;
    margin-top: 16px;
  }
  .detail-msg {
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
    background: var(--surface-2);
    padding: 10px;
    border-radius: var(--radius);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .facts {
    margin: 14px 0;
  }
  .recovery {
    margin-bottom: 16px;
  }
  .payloads-label {
    margin-bottom: 8px;
  }
  details {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 8px 10px;
    margin-bottom: 6px;
  }
  summary {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    font-size: var(--text-xs);
  }
  pre {
    font-size: var(--text-2xs);
    overflow-x: auto;
    margin: 8px 0 0;
  }
  .muted {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
