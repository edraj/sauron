<script lang="ts">
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
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
    fileLabel,
    formTitle,
    isDart as kindIsDart,
    resetAfterUpload,
    uploadMessage,
    type ArtifactKind,
    type DartPlatform,
    type UploadForm,
  } from '../lib/models/artifact-upload';

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
  const error = $derived(view.error);

  // artifacts.rs:89,222 — both upload and delete authorize at the app. Listing
  // needs only `issue:read` (artifacts.rs:189), which is why the list itself
  // stays readable while these two lock.
  const writeLock = $derived(
    lockedBy('artifact:write', { app: sessionStore.currentAppId, level: 'app' }),
  );

  // Upload form. Handles both artifact kinds the API accepts: JavaScript source
  // maps and Flutter/Dart symbol ELFs. Which fields are shown, what the file
  // input accepts and what is sent all follow `kind` — see
  // `models/artifact-upload.ts`, where that mapping lives so it can be tested.
  let kind = $state<ArtifactKind>('js_sourcemap');
  let dartPlatform = $state<DartPlatform>('android');
  let release = $state('');
  let name = $state('');
  let arch = $state('');
  let file = $state<File | null>(null);
  let uploading = $state(false);

  const isDart = $derived(kindIsDart(kind));

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
    const sent: UploadForm = { kind, dartPlatform, release, name, arch };
    uploading = true;
    try {
      const res = await uploadArtifact(appId, file, buildUploadParams(sent));
      toastStore.push(uploadMessage(sent.kind, res), 'success');
      const next = resetAfterUpload(sent);
      release = next.release;
      name = next.name;
      arch = next.arch;
      clearFile();
      // Prefix-wide, not just this key. The key carries `scopeKey`
      // (`appId:envId`), so this app has one cache entry PER ENVIRONMENT even
      // though the endpoint takes no environment argument. A forced reload
      // refreshes only the entry for the environment currently selected;
      // switching environments afterwards would paint the pre-mutation copy.
      viewCache.invalidate('sourcemaps.artifacts');
      await load(appId, true);
    } catch (e) {
      toastStore.push((e as Error).message, 'error');
    } finally {
      uploading = false;
    }
  }

  async function remove(id: string) {
    const appId = sessionStore.currentAppId;
    if (!appId) return;
    try {
      await deleteArtifact(appId, id);
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
      toastStore.push((e as Error).message, 'error');
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const u = ['KB', 'MB', 'GB'];
    let v = n / 1024,
      i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(1)} ${u[i]}`;
  }

  function fmtDate(s: string): string {
    return new Date(s).toLocaleString();
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

<AdminShell requireProject>
  <div class="page">
    <header class="head">
      <div>
        <h1 class="page-title">Source Maps</h1>
        <p class="sub muted">
          Upload JavaScript source maps and Flutter symbol files so minified and obfuscated stack
          traces resolve to your original code.
        </p>
      </div>
    </header>

    <Card>
      {#snippet header()}<h3 class="card-title-inline">{formTitle(kind)}</h3>{/snippet}
      <div class="upload">
        <div class="fields">
          <div class="field">
            <label class="lbl" for="art-kind">Kind</label>
            <div class="control select">
              <!-- Locked while a request is in flight: switching kind mid-upload
                   changes which fields are on screen (and clears the staged file)
                   under a request that was built from the old ones. -->
              <select id="art-kind" bind:value={kind} onchange={clearFile} disabled={uploading}>
                {#each ARTIFACT_KINDS as k (k.value)}
                  <option value={k.value}>{k.label}</option>
                {/each}
              </select>
              <span class="affix"><Icon name="chevron-down" size={15} /></span>
            </div>
          </div>

          {#if isDart}
            <div class="field">
              <label class="lbl" for="art-platform">Platform</label>
              <div class="control select">
                <select id="art-platform" bind:value={dartPlatform}>
                  {#each DART_PLATFORMS as p (p.value)}
                    <option value={p.value}>{p.label}</option>
                  {/each}
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>
            <Input bind:value={release} label="Release" placeholder="app@1.4.2+12" />
            <Input bind:value={arch} label="Arch (optional)" placeholder="arm64" />
          {:else}
            <Input bind:value={release} label="Release" placeholder="web@1.4.2" />
            <Input bind:value={name} label="Minified file path" placeholder="~/static/app.min.js" />
          {/if}
        </div>
        <label class="file-field">
          <span class="lbl">{fileLabel(kind)}</span>
          <input bind:this={fileInput} type="file" accept={fileAccept(kind)} onchange={onFile} />
        </label>
        <div class="actions">
          <Button
            variant="primary"
            disabled={!file || uploading}
            lockedReason={writeLock}
            onclick={upload}
          >
            {uploading ? 'Uploading…' : 'Upload'}
          </Button>
        </div>
      </div>
      {#if isDart}
        <p class="hint muted">
          The debug id is read out of the file's own build-id note — nothing to paste. Flutter emits
          these with <code class="mono">--split-debug-info</code>.
        </p>
      {/if}
      <p class="hint muted">
        Or from CI: <code class="mono">{cliHint(kind)}</code>
      </p>
    </Card>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    {#if loading}
      <div class="center"><Spinner /></div>
    {:else if artifacts.length === 0}
      <EmptyState
        title="No source maps or symbols yet"
        description="Upload a .map or a Flutter symbol file above, or wire the CLI into your deploy."
      />
    {:else}
      <DataTable>
        {#snippet head()}
          <tr>
            <th>Release</th>
            <th>File</th>
            <th>Platform</th>
            <th>Kind</th>
            <th class="num">Size</th>
            <th>Uploaded</th>
            <th></th>
          </tr>
        {/snippet}
        {#snippet children()}
          {#each artifacts as a (a.id)}
            <tr>
              <td class="mono">{a.release ?? '—'}</td>
              <td class="mono">{a.name ?? a.debug_id ?? '—'}</td>
              <td>{a.platform}{a.arch ? ` / ${a.arch}` : ''}</td>
              <td>{a.kind}</td>
              <td class="num">{fmtBytes(a.uncompressed_size)}</td>
              <td class="cell-muted">{fmtDate(a.created_at)}</td>
              <td>
                <Button variant="ghost" size="sm" lockedReason={writeLock} onclick={() => remove(a.id)}>
                  Delete
                </Button>
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    {/if}
  </div>
</AdminShell>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
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
  /* `:global` so it reaches both `Input`'s `.field` and the native-control
     wrappers below, which are page-scoped. */
  .fields :global(.field) {
    flex: 1 1 160px;
  }

  /* Native `<select>` styled to match the Input component — `lib/components/ui/`
     has no Select primitive, so this is the idiom (see `Monitors.svelte`). */
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
  /* The Kind picker locks during an upload; say so visually rather than leaving
     a control that looks live and silently ignores clicks. */
  .control select:disabled {
    opacity: 0.55;
    cursor: not-allowed;
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
  .file-field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .file-field .lbl {
    font-size: 12px;
    font-weight: 550;
    color: var(--text-muted);
  }
  .hint {
    margin-top: 12px;
    font-size: 12px;
  }
  .hint code {
    font-size: 11px;
    background: var(--surface-2);
    padding: 2px 6px;
    border-radius: var(--radius);
  }
  .center {
    display: flex;
    justify-content: center;
    padding: 40px;
  }
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--danger, #e5484d) 12%, transparent);
    color: var(--danger, #e5484d);
    font-size: 13px;
  }
</style>
