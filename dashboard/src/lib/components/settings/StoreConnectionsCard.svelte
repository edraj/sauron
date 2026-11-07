<script lang="ts">
  import { t, type MessageKey } from '../../i18n';
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Skeleton from '../ui/Skeleton.svelte';
  import Input from '../ui/Input.svelte';
  import Textarea from '../ui/Textarea.svelte';
  import Select from '../ui/Select.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import ErrorState from '../ui/ErrorState.svelte';
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
  const STORE_FIELDS: Record<StoreKind, { key: string; label: MessageKey; hint?: MessageKey }[]> = {
    google_play: [
      { key: 'package_name', label: 'stores.field.packageName', hint: 'stores.field.packageNameHint' },
      { key: 'gcs_bucket', label: 'stores.field.bucket', hint: 'stores.field.bucketHint' },
    ],
    app_store: [
      { key: 'bundle_id', label: 'stores.field.bundleId' },
      { key: 'apple_app_id', label: 'stores.field.appleAppId', hint: 'stores.field.appleAppIdHint' },
      { key: 'issuer_id', label: 'stores.field.issuerId' },
      { key: 'key_id', label: 'stores.field.keyId' },
      { key: 'vendor_number', label: 'stores.field.vendorNumber' },
    ],
  };

  // Store names are product names and stay as they are; the rest is copy.
  const STORE_META: Record<StoreKind, { name: string; secretLabel: MessageKey; secretHint: MessageKey }> = {
    google_play: {
      name: 'Google Play',
      secretLabel: 'stores.secret.playLabel',
      secretHint: 'stores.secret.playHint',
    },
    app_store: {
      name: 'App Store',
      secretLabel: 'stores.secret.appleLabel',
      secretHint: 'stores.secret.appleHint',
    },
  };

  const STORES: StoreKind[] = ['google_play', 'app_store'];

  let loading = $state(true);
  // The caught error itself, so ErrorState can say what kind of failure it was.
  let loadError = $state<unknown>(null);
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
      loadError = err;
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
      toastStore.success(t('stores.toast.saved', { store: STORE_META[store].name }));
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
      toastStore.success(t('stores.toast.removed', { store: STORE_META[store].name }));
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
      toastStore.success(t('stores.toast.queued'));
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
      toastStore.success(value ? t('stores.toast.envSet') : t('stores.toast.envCleared'));
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      savingEnv = false;
    }
  }

  function statusLine(c: StoreConnection | null): { text: string; tone: 'muted' | 'error' } {
    if (!c) return { text: t('stores.status.notConfigured'), tone: 'muted' };
    switch (c.state) {
      case 'error':
        return { text: c.last_error ?? t('stores.status.failed'), tone: 'error' };
      case 'never_synced':
        return { text: t('stores.status.neverSynced'), tone: 'muted' };
      case 'pending':
        return { text: t('stores.status.pending'), tone: 'muted' };
      default:
        return {
          text: c.last_synced_at
            ? t('stores.status.synced', { when: relativeTime(c.last_synced_at) })
            : t('stores.status.syncedPlain'),
          tone: 'muted',
        };
    }
  }
</script>

<Card title={t('stores.title')}>
  <p class="card-desc muted">
    {t('prose.stores.lede')}
  </p>

  {#if loading}
    <Skeleton rows={4} />
  {:else if loadError}
    <ErrorState
      size="section"
      title={t('common.couldntLoad', { thing: t('stores.thing') })}
      error={loadError}
      onretry={() => void load()}
    />
  {:else}
    <div class="field env-field">
      <Select
        id="store-env"
        label={t('stores.environment')}
        hint={t('prose.stores.environment')}
        value={app.store_environment_id ?? ''}
        options={[
          { value: '', label: t('stores.none') },
          ...environments.map((env) => ({ value: env.id, label: env.name })),
        ]}
        disabled={savingEnv || !!writeLock}
        onchange={(v) => saveEnvironment(v)}
      />
    </div>

    {#each STORES as store (store)}
      {@const c = connectionFor(store)}
      {@const status = statusLine(c)}
      <section class="store">
        <header class="store-head">
          <h4 class="store-name">{STORE_META[store].name}</h4>
          <span class="status {status.tone}">{status.text}</span>
        </header>

        {#each STORE_FIELDS[store] as f (f.key)}
          <!-- type="text" for every field, including vendor_number. See the
               STORE_FIELDS comment: a number input freezes the DOM here. -->
          <div class="field">
            <Input
              id="{store}-{f.key}"
              label={t(f.label)}
              hint={f.hint ? t(f.hint) : undefined}
              autocomplete="off"
              bind:value={drafts[store][f.key]}
              disabled={!!writeLock}
            />
          </div>
        {/each}

        <div class="field">
          <Textarea
            id="{store}-secret"
            label={t(STORE_META[store].secretLabel)}
            mono
            spellcheck={false}
            autocomplete="off"
            placeholder={c?.has_secret ? t('stores.secret.storedPlaceholder') : ''}
            hint={c?.has_secret
              ? `${t(STORE_META[store].secretHint)} ${t('stores.secret.keepHint')}`
              : t(STORE_META[store].secretHint)}
            bind:value={secrets[store]}
            disabled={!!writeLock}
          />
        </div>

        <div class="actions">
          <!-- `secondary`: two stores, two Saves, and the page above may
               already hold its one primary. -->
          <Button
            loading={saving === store}
            disabled={!complete(store)}
            lockedReason={writeLock}
            onclick={() => save(store)}
          >
            {t('common.save')}
          </Button>
          {#if c}
            <Button lockedReason={writeLock} onclick={() => sync(store)}>{t('stores.queueSync')}</Button>
            <Button variant="ghost" lockedReason={writeLock} onclick={() => (confirmRemove = store)}>
              {t('common.remove')}
            </Button>
          {/if}
        </div>
      </section>
    {/each}
  {/if}
</Card>

<ConfirmDialog
  open={confirmRemove !== null}
  title={t('stores.confirmRemoveTitle', {
    store: confirmRemove ? STORE_META[confirmRemove as StoreKind].name : '',
  })}
  message={t('prose.stores.remove')}
  confirmLabel={t('stores.removeAction')}
  danger
  loading={removing !== null}
  onconfirm={() => confirmRemove && void remove(confirmRemove as StoreKind)}
  oncancel={() => (confirmRemove = null)}
/>

<style>
  .card-desc {
    font-size: var(--text-sm);
    margin-bottom: 16px;
    line-height: 1.55;
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
  .store-name {
    font-size: var(--text-sm);
    font-weight: var(--weight-strong);
    margin: 0;
  }
  .status {
    font-size: var(--text-xs);
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
  .env-field :global(.field) {
    display: flex;
    max-width: 360px;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
</style>
