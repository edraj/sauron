<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    listArtifacts,
    uploadArtifact,
    deleteArtifact,
    type SymbolArtifact,
  } from '../lib/api/artifacts';

  let artifacts = $state<SymbolArtifact[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Upload form (JS source maps; Dart symbols upload via the CLI).
  let release = $state('');
  let name = $state('');
  let file = $state<File | null>(null);
  let uploading = $state(false);

  function onFile(e: Event) {
    file = (e.target as HTMLInputElement).files?.[0] ?? null;
  }

  async function load(appId: string) {
    loading = true;
    error = null;
    try {
      artifacts = await listArtifacts(appId);
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  }

  async function upload() {
    const appId = sessionStore.currentAppId;
    if (!appId || !file) return;
    uploading = true;
    try {
      const res = await uploadArtifact(appId, file, {
        kind: 'js_sourcemap',
        platform: 'web',
        release: release.trim() || undefined,
        name: name.trim() || undefined,
      });
      toastStore.push(res.deduped ? 'Already uploaded (deduped)' : 'Source map uploaded', 'success');
      release = '';
      name = '';
      file = null;
      await load(appId);
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
      artifacts = artifacts.filter((a) => a.id !== id);
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
    if (appId) void load(appId);
  });
</script>

<AppShell>
  <div class="page">
    <header class="head">
      <div>
        <h1 class="page-title">Source Maps</h1>
        <p class="sub muted">
          Upload JavaScript source maps so minified stack traces resolve to your original code.
        </p>
      </div>
    </header>

    <Card>
      {#snippet header()}<h3 class="card-title-inline">Upload a source map</h3>{/snippet}
      <div class="upload">
        <div class="fields">
          <Input bind:value={release} label="Release" placeholder="web@1.4.2" />
          <Input bind:value={name} label="Minified file path" placeholder="~/static/app.min.js" />
        </div>
        <label class="file-field">
          <span class="lbl">Source map (.map)</span>
          <input type="file" accept=".map,application/json" onchange={onFile} />
        </label>
        <div class="actions">
          <Button variant="primary" disabled={!file || uploading} onclick={upload}>
            {uploading ? 'Uploading…' : 'Upload'}
          </Button>
        </div>
      </div>
      <p class="hint muted">
        Or from CI: <code class="mono"
          >sauron-symcli upload-sourcemap --api &lt;url&gt; --token &lt;jwt&gt; --app &lt;id&gt; --release
          &lt;r&gt; --name &lt;path&gt; app.min.js.map</code
        >
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
        title="No source maps yet"
        description="Upload a .map above, or wire the CLI into your deploy."
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
                <Button variant="ghost" size="sm" onclick={() => remove(a.id)}>Delete</Button>
              </td>
            </tr>
          {/each}
        {/snippet}
      </DataTable>
    {/if}
  </div>
</AppShell>

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
  .fields {
    display: flex;
    gap: 14px;
    flex: 1;
    min-width: 260px;
  }
  .fields :global(.field) {
    flex: 1;
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
