<script lang="ts">
  import { loadErrorText } from '../lib/models/load-error';
  import { formatNumber, labelFor, t, type MessageKey } from '../lib/i18n';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Select from '../lib/components/ui/Select.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import ErrorState from '../lib/components/ui/ErrorState.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import PageHeader from '../lib/components/layout/PageHeader.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import ClientPager from '../lib/components/ClientPager.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { pageRefresher } from '../lib/stores/page-refresh.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    listArtifacts,
    uploadArtifact,
    deleteArtifact,
    type SymbolArtifact,
  } from '../lib/api/artifacts';
  import {
    ARTIFACT_KINDS,
    DART_PLATFORMS,
    buildUploadParams,
    cliHint,
    fileAccept,
    isDart as kindIsDart,
    requiresDebugId,
    resetAfterUpload,
    uploadMessage,
    type ArtifactKind,
    type DartPlatform,
    type UploadForm,
  } from '../lib/models/artifact-upload';
  import { dartCoverageGaps, type DartBuildCoverage } from '../lib/models/dart-coverage';
  import { setOffsetPage, setOffsetSort, type OffsetListState } from '../lib/models/list-state';
  import { ARTIFACT_DEFAULT_SORT, artifactAccessor } from '../lib/models/artifact-sort';
  import { pageSlice } from '../lib/models/paginate';
  import { sortRows } from '../lib/models/sort-rows';
  import type { SortDir } from '../lib/models/sort';
  import Freshness from '../lib/components/ui/Freshness.svelte';

  /** Rows per page. The list arrives whole, so this is a rendering budget only. */
  const PAGE = 25;

  // Cached view (lib/stores/cached-view.svelte.ts): the artifact list paints from
  // cache on return instead of blanking to a spinner, then refreshes behind it.
  // Re-exposed under the names the template already uses, so the markup is
  // unchanged.
  //
  // `artifacts` is a SHARED reference into the cache — never edit through it
  // (see `remove` below, which refetches rather than splicing).
  const view = new CachedView<SymbolArtifact[]>();

  const artifacts = $derived(view.data ?? []);
  const loading = $derived(view.loading);

  // Human words for the artifact kind, everywhere it is shown: the picker, the
  // form's title and file label, and the table's Kind column (which used to
  // print the raw API value, `js_sourcemap`).
  const KIND_LABELS: Record<ArtifactKind, MessageKey> = {
    js_sourcemap: 'sourcemaps.kind.jsSourcemap',
    dart_symbols: 'sourcemaps.kind.dartSymbols',
    dart_obfuscation_map: 'sourcemaps.kind.dartObfuscationMap',
  };
  const FORM_TITLES: Record<ArtifactKind, MessageKey> = {
    js_sourcemap: 'sourcemaps.form.jsSourcemap',
    dart_symbols: 'sourcemaps.form.dartSymbols',
    dart_obfuscation_map: 'sourcemaps.form.dartObfuscationMap',
  };
  const FILE_LABELS: Record<ArtifactKind, MessageKey> = {
    js_sourcemap: 'sourcemaps.file.jsSourcemap',
    dart_symbols: 'sourcemaps.file.dartSymbols',
    dart_obfuscation_map: 'sourcemaps.file.dartObfuscationMap',
  };
  const kindLabel = (k: string): string => labelFor(KIND_LABELS, k);
  const kindOptions = $derived(ARTIFACT_KINDS.map((k) => ({ value: k.value, label: kindLabel(k.value) })));
  const platformOptions = DART_PLATFORMS.map((p) => ({ value: p.value, label: p.label }));

  // Delete is irreversible (traces for that release stop being de-obfuscated),
  // so it is confirmed. The row being confirmed, or null.
  let confirmDelete = $state<SymbolArtifact | null>(null);
  let deleting = $state(false);

  // artifacts.rs:89,222 — both upload and delete authorize at the app. Listing
  // needs only `issue:read` (artifacts.rs:189), which is why the list itself
  // stays readable while these two lock.
  const writeLock = $derived(
    lockedBy('artifact:write', { app: sessionStore.currentAppId, level: 'app' }),
  );

  // `/v1/apps/{id}/artifacts` returns every artifact in one response, so the
  // sort and the pager both run here, over the SAME array: order the whole list
  // first, then take a window out of it. Sorting the window instead would
  // reorder only what is on screen while presenting itself as having ordered
  // everything.
  //
  // Sort and offset are one `OffsetListState` rather than two variables because
  // `setOffsetSort` resets the offset as part of applying a sort — a re-ordered
  // list makes the current window meaningless, so page 1 is the only honest
  // place to land.
  let list = $state<OffsetListState>({ sort: ARTIFACT_DEFAULT_SORT, offset: 0 });

  // `sortRows` copies before sorting. That is load-bearing here and not merely
  // tidy: `view.data` is the VERY ARRAY the view cache holds, handed back by
  // reference (this file's own header comment says so, and `$state.raw` keeps
  // that identity exact), so an in-place sort would reorder the cached payload
  // for every later reader and the ordering would survive into the next visit.
  const sorted = $derived(sortRows(artifacts, artifactAccessor(list.sort.key), list.sort.dir));
  const page = $derived(pageSlice(sorted, list.offset, PAGE));

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  // Upload form. Handles both artifact kinds the API accepts: JavaScript source
  // maps and Flutter/Dart symbol ELFs. Which fields are shown, what the file
  // input accepts and what is sent all follow `kind` — see
  // `models/artifact-upload.ts`, where that mapping lives so it can be tested.
  let kind = $state<ArtifactKind>('js_sourcemap');
  let dartPlatform = $state<DartPlatform>('android');
  let release = $state('');
  let name = $state('');
  let arch = $state('');
  let debugId = $state('');
  let file = $state<File | null>(null);
  let uploading = $state(false);

  const isDart = $derived(kindIsDart(kind));
  const needsDebugId = $derived(requiresDebugId(kind));

  /**
   * Dart builds missing one of their two artifacts.
   *
   * Derived from the list already on screen — no extra request, and no query
   * over `error_events` to work out which builds were obfuscated. See
   * `models/dart-coverage.ts` for why stating what IS uploaded beats guessing
   * whether a map is needed.
   */
  const coverageGaps = $derived<DartBuildCoverage[]>(dartCoverageGaps(artifacts));

  /** Prefill the upload form for a build that is missing its map. */
  function uploadMapFor(build: DartBuildCoverage) {
    kind = 'dart_obfuscation_map';
    if (build.platform === 'android' || build.platform === 'ios') dartPlatform = build.platform;
    debugId = build.debugId;
    clearFile();
  }

  let fileInput = $state<HTMLInputElement | null>(null);

  function onFile(e: Event) {
    file = (e.target as HTMLInputElement).files?.[0] ?? null;
  }

  // Unstage the picked file, clearing the native input's filename too (setting
  // `file = null` alone leaves the old name on screen). Called after a
  // successful upload, and on a kind change: the `accept` filter changes with
  // the kind, so a `.map` chosen under "JavaScript source map" would otherwise
  // stay silently staged and get uploaded as `dart_symbols`, which the server
  // can only reject for having no build-id note.
  function clearFile() {
    file = null;
    if (fileInput) fileInput.value = '';
  }

  /**
   * `force` bypasses the fresh-window short-circuit — an upload or a delete has
   * to reach the network, or the list hands back the state from before it.
   *
   * `scopeKey` is in the key because it carries the selected environment, which
   * the axios interceptor puts on the request but which appears in no argument
   * here. Omit it and one environment's list can be served as another's.
   */
  const refresher = pageRefresher(async () => {
    const appId = sessionStore.currentAppId;
    if (appId) await load(appId, true);
  });

  async function load(appId: string, force = false) {
    await view.load(
      viewKey('sourcemaps.artifacts', appId, sessionStore.scopeKey),
      () => listArtifacts(appId),
      force,
    );
  }

  async function upload() {
    const appId = sessionStore.currentAppId;
    if (!appId || !file) return;
    // Snapshot the form before awaiting. Everything after the await — the toast's
    // wording and which fields are cleared — has to describe the request that was
    // sent, not whatever the controls read by the time it comes back. The Kind
    // select is disabled while `uploading`, so this is belt and braces; but the
    // guard that matters is the one that does not depend on the template.
    const sent: UploadForm = { kind, dartPlatform, release, name, arch, debugId };
    uploading = true;
    try {
      const res = await uploadArtifact(appId, file, buildUploadParams(sent));
      toastStore.success(uploadMessage(sent.kind, res));
      const next = resetAfterUpload(sent);
      release = next.release;
      name = next.name;
      arch = next.arch;
      debugId = next.debugId;
      clearFile();
      // Prefix-wide, not just this key. The key carries `scopeKey`
      // (`appId:envId`), so this app has one cache entry PER ENVIRONMENT even
      // though the endpoint takes no environment argument. A forced reload
      // refreshes only the entry for the environment currently selected;
      // switching environments afterwards would paint the pre-mutation copy.
      viewCache.invalidate('sourcemaps.artifacts');
      await load(appId, true);
    } catch (e) {
      toastStore.error((e as Error).message);
    } finally {
      uploading = false;
    }
  }

  async function remove(id: string) {
    const appId = sessionStore.currentAppId;
    if (!appId) return;
    deleting = true;
    try {
      await deleteArtifact(appId, id);
      confirmDelete = null;
      // Refetch instead of splicing locally: `artifacts` now points at the
      // cached payload, and editing through that shared reference would corrupt
      // it for every later reader. `force` so the fresh window can't hand back
      // the row that was just deleted. Rows stay on screen while it runs (a
      // cache hit means `loading` never flips), so there is no spinner flash.
      // Prefix-wide, not just this key. The key carries `scopeKey`
      // (`appId:envId`), so this app has one cache entry PER ENVIRONMENT even
      // though the endpoint takes no environment argument. A forced reload
      // refreshes only the entry for the environment currently selected;
      // switching environments afterwards would paint the pre-mutation copy.
      viewCache.invalidate('sourcemaps.artifacts');
      await load(appId, true);
    } catch (e) {
      toastStore.error((e as Error).message);
    } finally {
      deleting = false;
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${formatNumber(n)} B`;
    const u = ['KB', 'MB', 'GB'];
    let v = n / 1024,
      i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(1)} ${u[i]}`;
  }

  $effect(() => {
    const appId = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes — it is
    // part of the cache key, so without this the page would keep showing the
    // payload fetched under the previous scope.
    sessionStore.scopeKey;
    if (appId) void load(appId);
  });
</script>

<AdminShell>
  <PageHeader title={t('admin.nav.sourceMaps')} subtitle={t('prose.sourcemaps.subtitle')}>
    {#snippet freshness()}
      <Freshness fetchedAt={view.fetchedAt} revalidating={view.revalidating} />
    {/snippet}
    {#snippet refresh()}
      <RefreshButton onclick={refresher.run} loading={refresher.busy || view.revalidating} />
    {/snippet}
  </PageHeader>

  <div class="page">

    <Card title={t(FORM_TITLES[kind])}>
      <div class="upload">
        <div class="fields">
          <!-- Locked while a request is in flight: switching kind mid-upload
               changes which fields are on screen (and clears the staged file)
               under a request that was built from the old ones. -->
          <Select
            id="art-kind"
            label={t('sourcemaps.column.kind')}
            value={kind}
            options={kindOptions}
            disabled={uploading}
            onchange={(v) => {
              kind = v as ArtifactKind;
              clearFile();
            }}
          />

          {#if isDart}
            <Select
              id="art-platform"
              label={t('sourcemaps.column.platform')}
              value={dartPlatform}
              options={platformOptions}
              onchange={(v) => (dartPlatform = v as DartPlatform)}
            />
            <Input bind:value={release} label={t('sourcemaps.column.release')} placeholder="app@1.4.2+12" />
            {#if needsDebugId}
              <!--
                The one form with a debug-id input. A map is plain JSON with no
                build-id note to read, so nothing can derive it and the server
                refuses the upload without one — see `artifact-upload.ts`.
              -->
              <Input
                bind:value={debugId}
                label={t('sourcemaps.field.debugId')}
                placeholder={t('prose.placeholder.debugId')}
              />
            {:else}
              <Input bind:value={arch} label={t('sourcemaps.field.arch')} placeholder="arm64" />
            {/if}
          {:else}
            <Input bind:value={release} label={t('sourcemaps.column.release')} placeholder="web@1.4.2" />
            <Input bind:value={name} label={t('sourcemaps.field.minifiedPath')} placeholder="~/static/app.min.js" />
          {/if}
        </div>
        <label class="file-field">
          <span class="lbl">{t(FILE_LABELS[kind])}</span>
          <input bind:this={fileInput} type="file" accept={fileAccept(kind)} onchange={onFile} />
        </label>
        <div class="actions">
          <Button
            variant="primary"
            loading={uploading}
            disabled={!file || (needsDebugId && debugId.trim() === '')}
            lockedReason={writeLock}
            onclick={upload}
          >
            {t('common.upload')}
          </Button>
        </div>
      </div>
      {#if needsDebugId}
        <p class="hint muted">
          {t('prose.sourcemaps.sameId.a')} <b>{t('prose.sourcemaps.sameId.b')}</b>
          {t('prose.sourcemaps.sameId.c')}
          <code class="mono">--extra-gen-snapshot-options=--save-obfuscation-map</code>.
        </p>
      {:else if isDart}
        <p class="hint muted">
          {t('prose.sourcemaps.debugId.a')}
          <code class="mono">--split-debug-info</code>.
        </p>
      {/if}
      <p class="hint muted">
        {t('sourcemaps.orFromCi')} <code class="mono">{cliHint(kind)}</code>
      </p>
    </Card>

    <!--
      The gap between "symbols uploaded" and "readable crash". Shown only when a
      build is actually missing one of its two artifacts, so a fully-covered app
      never sees it and it stays worth reading when it appears.
    -->
    {#if coverageGaps.length > 0}
      <Card>
        {#snippet header()}
          <div class="cov-head">
            <span class="cov-icon" aria-hidden="true"><Icon name="triangle-alert" size={15} /></span>
            <h3 class="cov-title">{t('sourcemaps.incompleteDart')}</h3>
          </div>
        {/snippet}
        <p class="cov-lead muted">
          {t('prose.sourcemaps.coverage.a')} <b>2</b> {t('prose.sourcemaps.coverage.b')}
          <b>{t('sourcemaps.frames')}</b>{t('prose.sourcemaps.coverage.c')}
          <b>{t('sourcemaps.className')}</b>{t('prose.sourcemaps.coverage.d')}
          <code class="mono">runtimeType.toString()</code>{t('prose.sourcemaps.coverage.e')}
        </p>
        <div class="cov-list">
          {#each coverageGaps as b (b.debugId)}
            <div class="cov-row">
              <div class="cov-what">
                <code class="mono cov-id">{b.debugId}</code>
                <span class="cov-meta faint">
                  {b.platform ?? t('sourcemaps.unknownPlatform')}{b.arches.length ? ` · ${b.arches.join(', ')}` : ''}
                </span>
                <span class="cov-gap">
                  {b.hasSymbols && !b.hasObfuscationMap
                    ? t('sourcemaps.gap.needsMap')
                    : !b.hasSymbols && b.hasObfuscationMap
                      ? t('sourcemaps.gap.needsSymbols')
                      : t('sourcemaps.gap.complete')}
                </span>
              </div>
              {#if b.hasSymbols && !b.hasObfuscationMap}
                <Button variant="secondary" lockedReason={writeLock} onclick={() => uploadMapFor(b)}>
                  {t('sourcemaps.upload')}
                </Button>
              {/if}
            </div>
          {/each}
        </div>
        <p class="hint muted">
          {t('prose.sourcemaps.noMap.a')}
          <code class="mono">--obfuscate</code> {t('prose.sourcemaps.noMap.b')}
        </p>
      </Card>
    {/if}

    {#if view.error && view.data !== undefined}
      <!-- A forced Refresh failed over the list we already had: it stays. -->
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
        title={t('common.couldntLoad', { thing: t('sourcemaps.thing') })}
        error={view}
        onretry={refresher.run}
      />
    {:else if artifacts.length === 0}
      <EmptyState
        title={t('sourcemaps.empty.title')}
        description={t('sourcemaps.empty.body')}
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <SortableTh key="release" columnDefault="asc" sort={list.sort} {onsort}>
              {t('sourcemaps.column.release')}
            </SortableTh>
            <SortableTh key="file" columnDefault="asc" sort={list.sort} {onsort}>{t('sourcemaps.column.file')}</SortableTh>
            <SortableTh key="platform" columnDefault="asc" sort={list.sort} {onsort}>
              {t('sourcemaps.column.platform')}
            </SortableTh>
            <SortableTh key="kind" columnDefault="asc" sort={list.sort} {onsort}>{t('sourcemaps.column.kind')}</SortableTh>
            <SortableTh key="size" class="num" sort={list.sort} {onsort}>{t('storage.column.size')}</SortableTh>
            <SortableTh key="uploaded" sort={list.sort} {onsort}>{t('sourcemaps.column.uploaded')}</SortableTh>
            <!-- The Delete button's column has no value to order by. -->
            <th></th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each page.rows as a (a.id)}
            <tr>
              <td class="mono">{a.release ?? '—'}</td>
              <td class="mono">{a.name ?? a.debug_id ?? '—'}</td>
              <td>{a.platform}{a.arch ? ` / ${a.arch}` : ''}</td>
              <td>{kindLabel(a.kind)}</td>
              <td class="num">{fmtBytes(a.uncompressed_size)}</td>
              <td><TimeValue value={a.created_at} muted /></td>
              <td>
                <Button variant="ghost" size="sm" lockedReason={writeLock} onclick={() => (confirmDelete = a)}>
                  {t('common.delete')}
                </Button>
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>

      <!-- `total` is the length of the EXACT array handed to `pageSlice` above
           — `sorted`, the same expression, not "all the artifacts". The two
           must be the same array: a pager measuring a longer list than the one
           being sliced re-creates the enabled-Next-onto-an-empty-page bug that
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

  <ConfirmDialog
    open={confirmDelete !== null}
    title={t('sourcemaps.confirmDelete.title')}
    message={confirmDelete?.release
      ? t('sourcemaps.confirmDelete.body', { release: confirmDelete.release })
      : t('sourcemaps.confirmDelete.bodyNoRelease')}
    confirmLabel={t('sourcemaps.confirmDelete.action')}
    danger
    loading={deleting}
    onconfirm={() => confirmDelete && void remove(confirmDelete.id)}
    oncancel={() => (confirmDelete = null)}
  />
</AdminShell>

<style>
  .cov-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .cov-icon {
    display: inline-flex;
    color: var(--warning);
  }
  .cov-title {
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
  }
  .cov-lead {
    font-size: var(--text-sm);
    margin-bottom: 14px;
  }
  .cov-list {
    display: flex;
    flex-direction: column;
  }
  .cov-row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .cov-what {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    flex: 1;
  }
  .cov-id {
    font-size: var(--text-xs);
    /* A build id is 40 hex characters with no break opportunity in it; without
       this it forces the row wider than the card on a narrow viewport. */
    overflow-wrap: anywhere;
  }
  .cov-meta {
    font-size: var(--text-xs);
  }
  .cov-gap {
    font-size: var(--text-xs);
    color: var(--text-muted);
  }

  .page {
    display: flex;
    flex-direction: column;
    gap: var(--page-gap);
  }
  .upload {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 14px;
  }
  /* Metadata on its own row, file picker + Upload on the next. The kind picker
     is a third control in what used to be a two-field row, and Dart adds a
     fourth; letting them share the row with the file input squeezed the text
     inputs to a third of their width. */
  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    flex: 1 1 100%;
    min-width: 260px;
  }
  /* `:global` so it reaches the `.field` wrappers of both `Input` and
     `Select`, which are their own components. */
  .fields :global(.field) {
    flex: 1 1 160px;
  }
  .file-field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .file-field .lbl {
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    color: var(--text-muted);
  }
  .hint {
    margin-top: 12px;
    font-size: var(--text-xs);
  }
  .hint code {
    font-size: var(--text-2xs);
    background: var(--surface-2);
    padding: 2px 6px;
    border-radius: var(--radius-sm);
  }
</style>
