<script lang="ts">
  import { t } from '../lib/i18n';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
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

  // Actions report failures through the same banner as the list's own load
  // error, and a `$derived` cannot be assigned to — so actions keep their own
  // state and the template's `error` is the two folded together.
  let actionError = $state<string | null>(null);
  const error = $derived(actionError ?? view.error);

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
      actionError = e instanceof Error ? e.message : String(e);
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
      actionError = e instanceof Error ? e.message : String(e);
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
      const parts = [`Re-queued ${fmt(r.requeued)} event(s).`];
      if (r.failed > 0) parts.push(`${fmt(r.failed)} could not be re-queued.`);
      if (r.unrecoverable > 0)
        parts.push(`${fmt(r.unrecoverable)} were never retained and cannot be recovered.`);
      notice = parts.join(' ');
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      acting = false;
    }
  }

  async function doDrop() {
    if (!confirmDrop) return;
    acting = true;
    try {
      await dropIngestFailure(confirmDrop.id);
      notice = `Dropped ${describeKind(confirmDrop.error_kind)} permanently.`;
      confirmDrop = null;
      selected = null;
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
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
  <div class="head">
    <div>
      <h1>{t('failures.title')}</h1>
      <p class="sub">
        {t('prose.failures.lede')}
      </p>
    </div>
    <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
    <RefreshButton onclick={refresh} loading={loading || revalidating} />
  </div>

  <Card>
    <div class="filters">
      <label>
        <span>{t('common.status')}</span>
        <select bind:value={statusFilter}>
          <option value="failed">{t('failures.state.needsAttention')}</option>
          <option value="requeued">{t('failures.state.retrying')}</option>
          <option value="resolved">{t('failures.state.resolved')}</option>
          <option value="">{t('common.all')}</option>
        </select>
      </label>
    </div>
  </Card>

  {#if notice}
    <Card>
      <div class="notice"><Icon name="info" size={15} /> <span>{notice}</span></div>
    </Card>
  {/if}

  {#if loading}
    <Skeleton rows={6} />
  {:else if error}
    <EmptyState title={t('failures.error.load')} description={error} icon="triangle-alert" />
  {:else if failures.length === 0}
    <EmptyState
      title={statusFilter === 'failed' ? 'No failing ingest' : 'Nothing here'}
      description={statusFilter === 'failed'
        ? 'Every event the edge accepted has been persisted.'
        : 'No groups match this filter.'}
      icon="check"
    />
  {:else}
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
                <div class="loss">−{fmt(f.dropped)} lost</div>
              {/if}
            </td>
            <td><Badge tone={statusTone(f.status)} size="sm">{f.status}</Badge></td>
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
  {/if}
</AdminShell>

{#if selected}
  {@const rec = describeRecovery(selected)}
  <Modal open title={describeKind(selected.error_kind)} onclose={() => (selected = null)}>
    <div class="detail">
      <p class="detail-msg">{selected.error_message}</p>

      <div class="facts">
        <div><span>{t('failures.column.fingerprint')}</span><code>{selected.fingerprint}</code></div>
        <div><span>{t('nav.selectApp')}</span><strong>{selected.app_name || 'unknown'}</strong></div>
        <div><span>{t('explore.column.firstSeen')}</span><TimeValue value={selected.first_seen_at} /></div>
        <div><span>{t('explore.column.lastSeen')}</span><TimeValue value={selected.last_seen_at} /></div>
        <div>
          <span>{t('failures.state.autoRetried')}</span>
          <strong>{wasAutoRetried(selected.error_kind) ? 'Yes, 3 attempts' : 'No'}</strong>
        </div>
      </div>

      <div class="recovery {rec.level}">
        <Icon name={rec.level === 'full' ? 'check' : 'triangle-alert'} size={15} />
        <span>{rec.summary}</span>
      </div>

      <h3>{t('failures.retainedPayloads')}</h3>
      {#if payloadsLoading}
        <Skeleton rows={3} />
      {:else if payloads.length === 0}
        <p class="muted">{t('failures.noPayloads')}</p>
      {:else}
        {#each payloads as p (p.id)}
          <details>
            <summary>
              <TimeValue value={p.created_at} />
              {#if p.attempts > 0}<Badge tone="neutral" size="sm">{p.attempts} attempts</Badge>{/if}
            </summary>
            <pre>{JSON.stringify(p.payload, null, 2)}</pre>
          </details>
        {/each}
      {/if}
    </div>
  </Modal>
{/if}

{#if confirmDrop}
  <Modal open title={t('failures.confirmDrop')} onclose={() => (confirmDrop = null)}>
    <div class="confirm">
      <p>
        {t('prose.failures.deletes.a')} <strong>{fmt(confirmDrop.retained)}</strong> retained
        {confirmDrop.retained === 1 ? 'payload' : 'payloads'} for
        <strong>{describeKind(confirmDrop.error_kind)}</strong>. They cannot be recovered
        afterwards.
      </p>
      <p class="muted">
        {t('prose.failures.auditFirst')}
      </p>
      <div class="confirm-actions">
        <Button variant="ghost" onclick={() => (confirmDrop = null)}>{t('common.cancel')}</Button>
        <Button variant="danger" loading={acting} onclick={doDrop}>{t('failures.dropPermanently')}</Button>
      </div>
    </div>
  </Modal>
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
  }
  h1 {
    font-size: 20px;
    font-weight: 640;
    margin: 0 0 4px;
  }
  .sub {
    margin: 0;
    color: var(--text-muted);
    font-size: 13px;
    max-width: 68ch;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    align-items: flex-end;
  }
  .filters label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--text-muted);
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }
  .centered {
    display: flex;
    justify-content: center;
    padding: 48px 0;
  }
  .cause {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fp {
    font-size: 11px;
    color: var(--text-faint);
  }
  .msg {
    color: var(--text-muted);
    font-size: 12px;
    margin-top: 2px;
    max-width: 60ch;
  }
  .loss {
    font-size: 11px;
    color: var(--danger, #e5484d);
    font-weight: 600;
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
    font-family: var(--font-mono, monospace);
    font-size: 12px;
    background: var(--surface-2);
    padding: 10px;
    border-radius: var(--radius-md);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 10px;
    margin: 14px 0;
  }
  .facts div {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 13px;
  }
  .facts span {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
  }
  .recovery {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-radius: var(--radius-md);
    font-size: 13px;
    margin-bottom: 16px;
  }
  .recovery.full {
    background: color-mix(in srgb, var(--success, #30a46c) 12%, transparent);
  }
  .recovery.partial,
  .recovery.none {
    background: color-mix(in srgb, var(--danger, #e5484d) 12%, transparent);
  }
  h3 {
    font-size: 13px;
    font-weight: 620;
    margin: 0 0 8px;
  }
  details {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 8px 10px;
    margin-bottom: 6px;
  }
  summary {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    font-size: 12px;
  }
  pre {
    font-size: 11px;
    overflow-x: auto;
    margin: 8px 0 0;
  }
  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
</style>
