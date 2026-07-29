<script lang="ts">
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Icon from '../ui/Icon.svelte';
  import Input from '../ui/Input.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import CopyButton from '../ui/CopyButton.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import Modal from '../ui/Modal.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';
  import { errorMessage } from '../../api/client';
  import { buildDsn, relativeTime, formatDateTime } from '../../utils/format';
  import {
    listEnvironments,
    createEnvironment,
    updateEnvironment,
    rotateEnvironmentKey,
    retireEnvironment,
  } from '../../api/environments';
  import type { Environment } from '../../models';

  interface Props {
    appId: string;
  }

  let { appId }: Props = $props();

  let envs = $state<Environment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showRetired = $state(false);
  let busyId = $state<string | null>(null);

  let creating = $state(false);
  let newName = $state('');
  let createBusy = $state(false);

  let renaming = $state<Environment | null>(null);
  let renameValue = $state('');
  let renameBusy = $state(false);

  let confirmRotate = $state<Environment | null>(null);
  let confirmRetire = $state<Environment | null>(null);

  // `sessionStore` is a class instance with plain fields, not a store contract —
  // reading it inside `$derived` (rather than latching once in `onMount`) is what
  // keeps these reactive to org/app switches. See the AppShell fix for the bug
  // this avoids.
  const canCreate = $derived(sessionStore.can('env:create', { app: appId }));
  const canUpdate = $derived(sessionStore.can('env:update', { app: appId }));
  const canRotate = $derived(sessionStore.can('env:rotate_key', { app: appId }));
  const canRetire = $derived(sessionStore.can('env:delete', { app: appId }));

  const active = $derived(envs.filter((e) => !e.retired_at));
  const retired = $derived(envs.filter((e) => e.retired_at));

  async function load() {
    loading = true;
    error = null;
    try {
      envs = await listEnvironments(appId, true);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (appId) void load();
  });

  /** Replace one row in place so the list does not jump while a row is busy. */
  function merge(updated: Environment) {
    envs = envs.map((e) => (e.id === updated.id ? updated : e));
  }

  async function submitCreate() {
    if (createBusy || !newName.trim()) return;
    createBusy = true;
    try {
      const created = await createEnvironment(appId, { name: newName.trim() });
      envs = [...envs, created].sort((a, b) => a.name.localeCompare(b.name));
      newName = '';
      creating = false;
      toastStore.success(`Environment "${created.name}" created.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      createBusy = false;
    }
  }

  async function submitRename() {
    const target = renaming;
    if (!target || !renameValue.trim()) return;
    busyId = target.id;
    renameBusy = true;
    try {
      merge(await updateEnvironment(target.id, { name: renameValue.trim() }));
      renaming = null;
      toastStore.success('Environment renamed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
      renameBusy = false;
    }
  }

  async function toggleIngest(env: Environment) {
    busyId = env.id;
    try {
      merge(await updateEnvironment(env.id, { ingest_enabled: !env.ingest_enabled }));
      toastStore.success(env.ingest_enabled ? 'Ingest muted.' : 'Ingest resumed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function promote(env: Environment) {
    busyId = env.id;
    try {
      const promoted = await updateEnvironment(env.id, { is_default: true });
      // A promote changes two rows: this one gains the flag, the previous default loses
      // it. Both are knowable here, so apply them locally rather than refetching.
      // Reloading would couple this action's success to a second request — and `load()`
      // swallows its own errors into the card's `error` state, so a failed GET would
      // show a success toast beside a card that had replaced its entire list with a
      // bare error string.
      envs = envs.map((e) =>
        e.id === promoted.id ? promoted : e.is_default ? { ...e, is_default: false } : e,
      );
      toastStore.success(`"${env.name}" is now the default environment.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function doRotate() {
    const target = confirmRotate;
    if (!target) return;
    busyId = target.id;
    try {
      merge(await rotateEnvironmentKey(target.id));
      confirmRotate = null;
      toastStore.success('Key rotated. Update this environment’s DSN everywhere.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function doRetire() {
    const target = confirmRetire;
    if (!target) return;
    busyId = target.id;
    try {
      merge(await retireEnvironment(target.id));
      confirmRetire = null;
      toastStore.success(`"${target.name}" retired. Its data stays queryable.`);
    } catch (err) {
      // A race with another admin can still trip the backend's "last live" or
      // "can't retire default" 409 even though we hide the button for those
      // cases locally — surface it rather than failing silently.
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }
</script>

<Card title="Environments">
  {#snippet actions()}
    {#if canCreate}
      <Button variant="secondary" size="sm" onclick={() => (creating = true)}>
        New environment
      </Button>
    {/if}
  {/snippet}

  {#if loading}
    <Spinner />
  {:else if error}
    <p class="err">{error}</p>
  {:else}
    <ul class="env-list">
      {#each active as env (env.id)}
        <li class="env" class:muted-row={!env.ingest_enabled}>
          <div class="head">
            <span class="name">{env.name}</span>
            {#if env.is_default}<Badge tone="info" size="sm">Default</Badge>{/if}
            {#if !env.ingest_enabled}<Badge tone="warning" size="sm">Muted</Badge>{/if}
            <span class="when muted" title={formatDateTime(env.created_at)}>
              created {relativeTime(env.created_at)}
            </span>
          </div>

          <div class="dsn">
            <code>{buildDsn(env.public_key, env.id)}</code>
            <CopyButton value={buildDsn(env.public_key, env.id)} size="sm" />
          </div>

          <div class="row-actions">
            {#if canUpdate}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => {
                  renaming = env;
                  renameValue = env.name;
                }}
              >
                Rename
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => toggleIngest(env)}
              >
                {env.ingest_enabled ? 'Mute ingest' : 'Resume ingest'}
              </Button>
              {#if !env.is_default}
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busyId === env.id}
                  onclick={() => promote(env)}
                >
                  Make default
                </Button>
              {/if}
            {/if}
            {#if canRotate}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => (confirmRotate = env)}
              >
                Rotate key
              </Button>
            {/if}
            {#if canRetire && !env.is_default && active.length > 1}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => (confirmRetire = env)}
              >
                Retire
              </Button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>

    {#if retired.length > 0}
      <div class="retired-toggle">
        <Button variant="ghost" size="sm" onclick={() => (showRetired = !showRetired)}>
          <Icon name={showRetired ? 'chevron-down' : 'chevron-right'} size={14} />
          {retired.length} retired
        </Button>
      </div>
      {#if showRetired}
        <ul class="env-list retired">
          {#each retired as env (env.id)}
            <li class="env">
              <div class="head">
                <span class="name">{env.name}</span>
                <Badge tone="neutral" size="sm">Retired</Badge>
                <span class="when muted" title={formatDateTime(env.retired_at ?? '')}>
                  retired {relativeTime(env.retired_at ?? '')}
                </span>
              </div>
              <p class="muted note">
                Ingest is off and its key no longer works. Existing data stays queryable.
              </p>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  {/if}
</Card>

<Modal bind:open={creating} title="New environment" size="sm">
  <Input
    label="Name"
    bind:value={newName}
    placeholder="staging"
    hint="Lowercase and short works best — this appears in every filter."
  />
  {#snippet footer()}
    <Button variant="secondary" disabled={createBusy} onclick={() => (creating = false)}>
      Cancel
    </Button>
    <Button loading={createBusy} disabled={!newName.trim()} onclick={submitCreate}>Create</Button>
  {/snippet}
</Modal>

<Modal
  open={renaming !== null}
  title="Rename environment"
  size="sm"
  onclose={() => (renaming = null)}
>
  <Input label="Name" bind:value={renameValue} />
  {#snippet footer()}
    <Button variant="secondary" onclick={() => (renaming = null)} disabled={renameBusy}>
      Cancel
    </Button>
    <Button loading={renameBusy} disabled={!renameValue.trim()} onclick={submitRename}>
      Save
    </Button>
  {/snippet}
</Modal>

<ConfirmDialog
  open={confirmRotate !== null}
  title="Rotate ingest key?"
  message={`Anything reporting to "${confirmRotate?.name ?? ''}" stops until its DSN is updated. There is no grace period.`}
  confirmLabel="Rotate"
  loading={busyId !== null && busyId === confirmRotate?.id}
  onconfirm={doRotate}
  oncancel={() => (confirmRotate = null)}
/>

<ConfirmDialog
  open={confirmRetire !== null}
  title="Retire environment?"
  message={`"${confirmRetire?.name ?? ''}" stops accepting events and leaves the picker. Its existing data stays queryable and is archived to cold storage on the normal schedule. This cannot be undone.`}
  confirmLabel="Retire"
  danger
  loading={busyId !== null && busyId === confirmRetire?.id}
  onconfirm={doRetire}
  oncancel={() => (confirmRetire = null)}
/>

<style>
  .env-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .env {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .muted-row {
    opacity: 0.7;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .name {
    font-weight: 600;
  }
  .when {
    margin-left: auto;
    font-size: 0.8rem;
  }
  .dsn {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-width: 0;
  }
  .dsn code {
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    white-space: nowrap;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    padding: 0.35rem 0.5rem;
    font-size: 0.8rem;
  }
  .row-actions {
    display: flex;
    gap: 0.25rem;
    flex-wrap: wrap;
  }
  .retired-toggle {
    margin-top: 0.75rem;
  }
  .retired {
    margin-top: 0.5rem;
  }
  .note {
    font-size: 0.8rem;
    margin: 0;
  }
  .err {
    color: var(--error);
  }
</style>
