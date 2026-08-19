<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatTime } from '../lib/utils/format';
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
  import SortableTh from '../lib/components/SortableTh.svelte';
  import ClientPager from '../lib/components/ClientPager.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { setOffsetPage, setOffsetSort, type OffsetListState } from '../lib/models/list-state';
  import { MONITOR_DEFAULT_SORT, monitorAccessor } from '../lib/models/monitor-sort';
  import { pageSlice } from '../lib/models/paginate';
  import { sortRows } from '../lib/models/sort-rows';
  import type { SortDir } from '../lib/models/sort';

  /** Rows per page. The list arrives whole, so this is a rendering budget only. */
  const PAGE = 25;

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

  // `/v1/projects/{id}/monitors` returns every monitor in one response, so the
  // sort and the pager both run here, over the SAME array: order the whole list
  // first, then take a window out of it. Sorting the window instead would
  // reorder only what is on screen while presenting itself as having ordered
  // everything.
  //
  // Sort and offset are one `OffsetListState` rather than two variables because
  // `setOffsetSort` resets the offset as part of applying a sort — a re-ordered
  // list makes the current window meaningless, so page 1 is the only honest
  // place to land.
  let list = $state<OffsetListState>({ sort: MONITOR_DEFAULT_SORT, offset: 0 });

  // `sortRows` copies before sorting. That is load-bearing here and not merely
  // tidy: `monitorsView.data` is the VERY ARRAY the view cache holds, handed
  // back by reference (`cached-view.svelte.ts` says so, and `$state.raw` keeps
  // that identity exact), so an in-place sort would reorder the cached payload
  // for every later reader and the ordering would survive into the next visit
  // to this page. No runes machinery prevents that — the same file notes
  // proxying is not a safety mechanism — only copying does.
  const sorted = $derived(sortRows(monitors, monitorAccessor(list.sort.key), list.sort.dir));
  // `page.rows` is the window; `sorted` stays the thing the pager measures.
  const page = $derived(pageSlice(sorted, list.offset, PAGE));

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

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

  // `formatTime` rather than a local `toLocaleTimeString([])`: the empty array
  // means "the browser's locale", which is not necessarily the one the rest of
  // the page is in.
  const fmtTime = (iso: string) => formatTime(iso);

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
        <h1 class="page-title">{t('monitors.column.uptime')}</h1>
        <p class="sub muted">{t('monitors.subtitle')}</p>
      </div>
      <div class="controls">
        {#if !showForm}
          <Button variant="primary" lockedReason={writeLock} onclick={openForm}>{t('monitors.new')}</Button>
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
      <Card title={t('monitors.new')}>
        <div class="form-grid">
          <Input label={t('common.name')} bind:value={name} placeholder={t('monitors.placeholder.name')} required />

          <div class="field">
            <label class="lbl" for="mon-kind">{t('monitors.column.type')}</label>
            <div class="control select">
              <select id="mon-kind" bind:value={kind}>
                <option value="http">{t('monitors.http')}</option>
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
              <label class="lbl" for="mon-method">{t('monitors.column.method')}</label>
              <div class="control select">
                <select id="mon-method" bind:value={method}>
                  <option>GET</option><option>POST</option><option>HEAD</option>
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>
          {:else}
            <div class="span-2">
              <Input label={t('monitors.hostPort')} bind:value={target} placeholder={t('monitors.placeholder.hostPort')} required />
            </div>
          {/if}

          <div class="field">
            <label class="lbl" for="mon-interval">{t('monitors.column.interval')}</label>
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
              label={t('monitors.webhookUrl')}
              bind:value={webhook}
              placeholder="https://hooks.example.com/…"
              hint={t('monitors.webhookHint')}
            />
          </div>
        </div>

        <div class="form-foot">
          <Button variant="ghost" onclick={closeForm}>{t('common.cancel')}</Button>
          <Button
            variant="primary"
            loading={saving}
            disabled={!name || !target}
            lockedReason={writeLock}
            onclick={submit}
          >
            {t('monitors.create')}
          </Button>
        </div>
      </Card>
    {/if}

    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if monitors.length === 0}
      <EmptyState
        title={t('monitors.empty.title')}
        description={t('monitors.empty.body')}
        icon="zap"
      >
        {#snippet action()}
          {#if !showForm}
            <Button variant="primary" lockedReason={writeLock} onclick={openForm}>{t('monitors.new')}</Button>
          {/if}
        {/snippet}
      </EmptyState>
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <SortableTh key="name" columnDefault="asc" sort={list.sort} {onsort}>{t('common.name')}</SortableTh>
            <SortableTh key="target" columnDefault="asc" sort={list.sort} {onsort}>
              {t('monitors.column.target')}
            </SortableTh>
            <!-- `desc` (the default), not `asc`: Status is a RANK — see
                 `MONITOR_STATUS_ORDER` — so it behaves like the count columns
                 beside it and the first click leads with the outages. `asc`
                 here would open the column with the healthy monitors on top. -->
            <SortableTh key="status" sort={list.sort} {onsort}>{t('common.status')}</SortableTh>
            <SortableTh key="uptime" class="num" sort={list.sort} {onsort}>{t('monitors.column.uptime24h')}</SortableTh>
            <SortableTh key="latency" class="num" sort={list.sort} {onsort}>{t('monitors.column.latency')}</SortableTh>
            <SortableTh key="checked" class="num" sort={list.sort} {onsort}>{t('monitors.column.checked')}</SortableTh>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each page.rows as m (m.id)}
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

      <!-- `total` is the length of the EXACT array handed to `pageSlice` above
           — `sorted`, the same expression, not "all the monitors". The two must
           be the same array: a pager measuring a longer list than the one being
           sliced re-creates the enabled-Next-onto-an-empty-page bug that
           `Pagination.hasNext` was made a required prop to kill. It is only
           because they agree that a final page of exactly PAGE rows correctly
           disables Next. (Nothing filters this table. If anything ever does,
           the filter must both feed `pageSlice` and be measured here, AND reset
           the offset with `setOffsetPage(list, 0)`.) -->
      <ClientPager
        offset={list.offset}
        limit={PAGE}
        total={sorted.length}
        onchange={(o) => (list = setOffsetPage(list, o))}
      />
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
    padding-inline-end: 34px;
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
    inset-inline-end: 11px;
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
