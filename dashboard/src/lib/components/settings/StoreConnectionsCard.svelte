<script lang="ts">
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import { lockedBy } from '../../models/page-access';
  import { errorMessage } from '../../api/client';
  import { toastStore } from '../../stores/toast.svelte';
  import { updateApp } from '../../api/apps';
  import { listEnvironments } from '../../api/environments';
  import {
    deleteStoreConnection,
    listStoreConnections,
    queueStoreSync,
    upsertStoreConnection,
    type StoreConnection,
    type StoreKind,
  } from '../../api/stores';
  import { relativeTime } from '../../utils/format';
  import type { App, AppEnvironment } from '../../models';

  interface Props {
    app: App;
    /** Called with the updated app after the store environment changes. */
    onAppUpdated: (app: App) => void;
  }

  let { app, onAppUpdated }: Props = $props();

  /**
   * The identifier fields each store needs, matching the backend's
   * `validate_identifiers` exactly — anything missing is a 400, so the form and
   * the validator have to agree.
   *
   * Every field is `type="text"`. `vendor_number` in particular must NEVER be
   * `type="number"`: `bind:value` on a number input writes back `number | null`,
   * and because the save button's `disabled` is itself a derived, computing the
   * guard is what throws — the DOM freezes while the button still looks
   * clickable. It is also an opaque identifier that can carry leading zeros,
   * which a number input would silently eat.
   */
  const STORE_FIELDS: Record<StoreKind, { key: string; label: string; hint?: string }[]> = {
    google_play: [
      { key: 'package_name', label: 'Package name', hint: 'e.g. com.example.app' },
      {
        key: 'gcs_bucket',
        label: 'Reports bucket',
        hint: 'From Play Console → Download reports. A gs:// prefix is fine.',
      },
    ],
    app_store: [
      { key: 'bundle_id', label: 'Bundle ID' },
      { key: 'apple_app_id', label: 'Apple app ID', hint: 'The numeric App Store id.' },
      { key: 'issuer_id', label: 'API key issuer ID' },
      { key: 'key_id', label: 'API key ID' },
      { key: 'vendor_number', label: 'Vendor number' },
    ],
  };

  const STORE_META: Record<StoreKind, { name: string; secretLabel: string; secretHint: string }> = {
    google_play: {
      name: 'Google Play',
      secretLabel: 'Service account JSON',
      secretHint:
        'The full JSON key file for a service account with read access to the reports bucket.',
    },
    app_store: {
      name: 'App Store',
      secretLabel: 'App Store Connect key (.p8)',
      secretHint: 'The contents of the AuthKey_XXXX.p8 file, including the BEGIN/END lines.',
    },
  };

  const STORES: StoreKind[] = ['google_play', 'app_store'];

  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let connections = $state<StoreConnection[]>([]);
  let environments = $state<AppEnvironment[]>([]);

  /** Draft identifier values per store, seeded from the server on load. */
  let drafts = $state<Record<string, Record<string, string>>>({});
  /**
   * Secret textareas. Always start EMPTY and are only sent when non-empty — an
   * untouched field must send no `secret` key at all, not `''`. The backend
   * rejects `''` rather than storing a credential that can never authenticate.
   */
  let secrets = $state<Record<string, string>>({});
  let saving = $state<string | null>(null);
  let removing = $state<string | null>(null);
  let confirmRemove = $state<string | null>(null);
  let savingEnv = $state(false);

  const writeLock = $derived(lockedBy('app:update', { app: app.id, level: 'app' }));

  function connectionFor(store: StoreKind): StoreConnection | null {
    return connections.find((c) => c.store === store) ?? null;
  }

  async function load() {
    loading = true;
    loadError = null;
    try {
      const [conns, envs] = await Promise.all([
        listStoreConnections(app.id),
        listEnvironments(app.id),
      ]);
      connections = conns;
      environments = envs;
      const next: Record<string, Record<string, string>> = {};
      for (const store of STORES) {
        const existing = conns.find((c) => c.store === store);
        next[store] = {};
        for (const f of STORE_FIELDS[store]) {
          next[store][f.key] = existing?.identifiers?.[f.key] ?? '';
        }
      }
      drafts = next;
    } catch (err) {
      loadError = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load();
  });

  function complete(store: StoreKind): boolean {
    const d = drafts[store] ?? {};
    return STORE_FIELDS[store].every((f) => (d[f.key] ?? '').trim().length > 0);
  }

  async function save(store: StoreKind) {
    if (saving) return;
    saving = store;
    try {
      const secret = (secrets[store] ?? '').trim();
      await upsertStoreConnection(app.id, store, {
        identifiers: drafts[store] ?? {},
        // Key omitted entirely when untouched — see `secrets` above.
        ...(secret ? { secret } : {}),
      });
      secrets[store] = '';
      toastStore.success(`${STORE_META[store].name} settings saved.`);
      await load();
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      saving = null;
    }
  }

  async function remove(store: StoreKind) {
    if (removing) return;
    removing = store;
    try {
      await deleteStoreConnection(app.id, store);
      confirmRemove = null;
      toastStore.success(`${STORE_META[store].name} credentials removed.`);
      await load();
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      removing = null;
    }
  }

  async function sync(store: StoreKind) {
    try {
      await queueStoreSync(app.id, store);
      // Deliberately not "Syncing…": the request only marks the connection due.
      // The daemon fetches on its next pass, so promising fresh data here would
      // be a lie told on every click.
      toastStore.success('Queued. The sync service will pick this up on its next pass.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    }
  }

  async function saveEnvironment(value: string) {
    if (savingEnv) return;
    savingEnv = true;
    try {
      const updated = await updateApp(app.id, {
        name: app.name,
        ingest_enabled: app.ingest_enabled,
        store_environment_id: value || null,
      });
      onAppUpdated(updated);
      toastStore.success(
        value ? 'Store environment set.' : 'Store environment cleared.',
      );
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      savingEnv = false;
    }
  }

  function statusLine(c: StoreConnection | null): { text: string; tone: 'muted' | 'error' } {
    if (!c) return { text: 'Not configured.', tone: 'muted' };
    switch (c.state) {
      case 'error':
        return { text: c.last_error ?? 'Last sync failed.', tone: 'error' };
      case 'never_synced':
        return { text: 'Waiting for the first sync.', tone: 'muted' };
      case 'pending':
        return {
          text: 'App Store is preparing this report. Apple usually takes 24–48 hours after setup.',
          tone: 'muted',
        };
      default:
        return {
          text: c.last_synced_at ? `Last synced ${relativeTime(c.last_synced_at)}.` : 'Synced.',
          tone: 'muted',
        };
    }
  }
</script>

<Card title="App stores">
  <p class="card-desc muted">
    Pull daily install and uninstall counts from Google Play and the App Store. Reports are daily
    and arrive one to three days late — this is the stores' own cadence, not a delay Sauron adds.
  </p>

  {#if loading}
    <div class="center"><Spinner size={22} /></div>
  {:else if loadError}
    <p class="err">{loadError}</p>
  {:else}
    <div class="field env-field">
      <label for="store-env">Store environment</label>
      <p class="hint">
        Which environment represents the build that ships to the stores. The Overview section
        appears only when this environment is selected. Store numbers themselves are app-wide —
        the stores report per package, not per environment.
      </p>
      <select
        id="store-env"
        value={app.store_environment_id ?? ''}
        disabled={savingEnv || !!writeLock}
        onchange={(e) => saveEnvironment((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="">None — hide the store section</option>
        {#each environments as env (env.id)}
          <option value={env.id}>{env.name}</option>
        {/each}
      </select>
    </div>

    {#each STORES as store (store)}
      {@const c = connectionFor(store)}
      {@const status = statusLine(c)}
      <section class="store">
        <header class="store-head">
          <h3>{STORE_META[store].name}</h3>
          <span class="status {status.tone}">{status.text}</span>
        </header>

        {#each STORE_FIELDS[store] as f (f.key)}
          <div class="field">
            <label for="{store}-{f.key}">{f.label}</label>
            <!-- type="text" for every field, including vendor_number. See the
                 STORE_FIELDS comment: a number input freezes the DOM here. -->
            <input
              id="{store}-{f.key}"
              type="text"
              autocomplete="off"
              bind:value={drafts[store][f.key]}
              disabled={!!writeLock}
            />
            {#if f.hint}<p class="hint">{f.hint}</p>{/if}
          </div>
        {/each}

        <div class="field">
          <label for="{store}-secret">{STORE_META[store].secretLabel}</label>
          <textarea
            id="{store}-secret"
            rows="3"
            spellcheck="false"
            autocomplete="off"
            placeholder={c?.has_secret ? 'Stored — paste a new value to replace it' : ''}
            bind:value={secrets[store]}
            disabled={!!writeLock}
          ></textarea>
          <p class="hint">
            {STORE_META[store].secretHint}
            {#if c?.has_secret}
              Leave blank to keep the stored credential.
            {/if}
          </p>
        </div>

        <div class="actions">
          <Button
            variant="primary"
            loading={saving === store}
            disabled={!complete(store)}
            lockedReason={writeLock}
            onclick={() => save(store)}
          >
            Save
          </Button>
          {#if c}
            <Button lockedReason={writeLock} onclick={() => sync(store)}>Queue sync</Button>
            {#if confirmRemove === store}
              <Button
                variant="danger"
                loading={removing === store}
                lockedReason={writeLock}
                onclick={() => remove(store)}
              >
                Yes, remove credentials
              </Button>
              <Button variant="ghost" onclick={() => (confirmRemove = null)}>Cancel</Button>
            {:else}
              <Button variant="ghost" onclick={() => (confirmRemove = store)}>Remove</Button>
            {/if}
          {/if}
        </div>

        {#if confirmRemove === store}
          <p class="hint warn">
            Removes the stored credentials and stops syncing. The install history already
            collected is kept, and reconnecting resumes against it.
          </p>
        {/if}
      </section>
    {/each}
  {/if}
</Card>

<style>
  .card-desc {
    font-size: 13px;
    margin-bottom: 16px;
    line-height: 1.55;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 32px;
  }
  .store {
    border-top: 1px solid var(--border);
    padding-top: 16px;
    margin-top: 16px;
  }
  .store-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
    flex-wrap: wrap;
  }
  .store-head h3 {
    font-size: 14px;
    font-weight: 600;
    margin: 0;
  }
  .status {
    font-size: 12px;
  }
  .status.muted {
    color: var(--text-muted);
  }
  .status.error {
    color: var(--error);
  }
  .field {
    margin-bottom: 12px;
  }
  .env-field {
    margin-bottom: 4px;
  }
  .field label {
    display: block;
    font-size: 12.5px;
    font-weight: 600;
    margin-bottom: 4px;
  }
  .field input,
  .field textarea,
  .field select {
    width: 100%;
    padding: 7px 9px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .field textarea {
    font-family: var(--font-mono, monospace);
    font-size: 12px;
    resize: vertical;
  }
  .hint {
    font-size: 11.5px;
    color: var(--text-muted);
    margin: 4px 0 0;
    line-height: 1.5;
  }
  .hint.warn {
    color: var(--warning);
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .err {
    color: var(--error);
    font-size: 13.5px;
  }
</style>
