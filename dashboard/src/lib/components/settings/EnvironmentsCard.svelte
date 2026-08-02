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
  import { lockedBy } from '../../models/page-access';
  import { toastStore } from '../../stores/toast.svelte';
  import { errorMessage } from '../../api/client';
  import { buildDsn, relativeTime, formatDateTime } from '../../utils/format';
  import {
    listEnvironments,
    createProjectEnvironment,
    renameProjectEnvironment,
    retireProjectEnvironment,
    updateAppEnvironment,
    rotateAppEnvironmentKey,
  } from '../../api/environments';
  import type { AppEnvironment, AppEnvironmentRow } from '../../models';

  // Every row below is an ENROLLMENT (`app_environments`) — this app's
  // membership in one of its project's environments — joined to the catalogue
  // name. Two ids therefore matter and must not be confused:
  //
  //   `env.id`             the enrollment. Its key, its mute switch, its
  //                        default flag, its DSN. Scoped to this app alone.
  //   `env.environment_id` the catalogue entry (`environments`), owned by the
  //                        project. Its NAME, and whether it exists at all.
  //                        Shared by every app in the project.
  //
  // So mute / promote / rotate go to `/v1/app-environments/{env.id}`, while
  // create / rename / retire go to the project catalogue and change what every
  // sibling app sees. The card says so out loud (see the intro line in the
  // markup and the confirm copy) — an admin renaming `staging` here is
  // renaming it for the whole project, and that must not be a surprise.
  interface Props {
    appId: string;
    /** The app's `project_id` — the catalogue create endpoint hangs off it. */
    projectId: string;
  }

  let { appId, projectId }: Props = $props();

  let envs = $state<AppEnvironment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showRetired = $state(false);
  let busyId = $state<string | null>(null);

  let creating = $state(false);
  let newName = $state('');
  let createBusy = $state(false);

  let renaming = $state<AppEnvironment | null>(null);
  let renameValue = $state('');
  let renameBusy = $state(false);

  let confirmRotate = $state<AppEnvironment | null>(null);
  let confirmRetire = $state<AppEnvironment | null>(null);

  // `sessionStore` is a class instance with plain fields, not a store contract —
  // reading it inside `$derived` (rather than latching once in `onMount`) is what
  // keeps these reactive to org/app switches. See the AppShell fix for the bug
  // this avoids.
  //
  // Catalogue actions resolve through the backend's `authorize_project`, so
  // they are asked at project scope (environments.rs:213,285,323); enrollment
  // actions resolve through `authorize_app` and are asked at app scope
  // (environments.rs:444,525). The explicit `level` is what makes those two
  // statements true: without it `can()` also ORs in `currentAppId`, so a purely
  // app-scoped grant lit up catalogue buttons the backend then answered with a
  // 403. That caveat used to be documented here as unavoidable; it isn't.
  const createLock = $derived(lockedBy('env:create', { project: projectId, level: 'project' }));
  const renameLock = $derived(lockedBy('env:update', { project: projectId, level: 'project' }));
  const retireLock = $derived(lockedBy('env:delete', { project: projectId, level: 'project' }));
  const updateLock = $derived(lockedBy('env:update', { app: appId, level: 'app' }));
  const rotateLock = $derived(lockedBy('env:rotate_key', { app: appId, level: 'app' }));

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

  /**
   * Refetch the rows without touching `loading`/`error`, and report whether it
   * worked instead of throwing. Used after a create, which is the one action
   * whose result this card cannot reconstruct locally: the catalogue POST
   * returns the catalogue row, but the enrollment (and therefore this app's
   * freshly minted key and DSN) is created server-side as a side effect. A
   * failed refetch must not clobber the list the way `load()` would — the
   * create itself already succeeded.
   */
  async function refetchRows(): Promise<boolean> {
    try {
      envs = await listEnvironments(appId, true);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Replace one row in place so the list does not jump while a row is busy.
   * The enrollment endpoints return the bare row with no `name` on it (the name
   * lives on the catalogue entry), so carry the existing one across rather than
   * blanking it.
   */
  function merge(updated: AppEnvironmentRow) {
    envs = envs.map((e) => (e.id === updated.id ? { ...updated, name: e.name } : e));
  }

  async function submitCreate() {
    if (createBusy || !newName.trim()) return;
    createBusy = true;
    try {
      const created = await createProjectEnvironment(projectId, { name: newName.trim() });
      newName = '';
      creating = false;
      const refreshed = await refetchRows();
      if (refreshed) {
        toastStore.success(`"${created.name}" added to every app in this project.`);
      } else {
        toastStore.success(
          `"${created.name}" added to every app in this project. Reload to see its ingest key.`,
        );
      }
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
      // Catalogue rename: `environment_id`, not `id`. Only the name changes, and
      // it changes for every app in the project — so patch the name onto every
      // local row sharing that catalogue entry rather than refetching.
      const renamed = await renameProjectEnvironment(target.environment_id, {
        name: renameValue.trim(),
      });
      envs = envs.map((e) =>
        e.environment_id === renamed.id ? { ...e, name: renamed.name } : e,
      );
      renaming = null;
      toastStore.success(`Renamed to "${renamed.name}" for every app in this project.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
      renameBusy = false;
    }
  }

  async function toggleIngest(env: AppEnvironment) {
    busyId = env.id;
    try {
      merge(await updateAppEnvironment(env.id, { ingest_enabled: !env.ingest_enabled }));
      toastStore.success(env.ingest_enabled ? 'Ingest muted.' : 'Ingest resumed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function promote(env: AppEnvironment) {
    busyId = env.id;
    try {
      const promoted = await updateAppEnvironment(env.id, { is_default: true });
      // A promote changes two rows: this one gains the flag, the previous default loses
      // it. Both are knowable here, so apply them locally rather than refetching.
      // Reloading would couple this action's success to a second request — and `load()`
      // swallows its own errors into the card's `error` state, so a failed GET would
      // show a success toast beside a card that had replaced its entire list with a
      // bare error string.
      envs = envs.map((e) =>
        e.id === promoted.id
          ? { ...promoted, name: e.name }
          : e.is_default
            ? { ...e, is_default: false }
            : e,
      );
      toastStore.success(`"${env.name}" is now this app's default environment.`);
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
      merge(await rotateAppEnvironmentKey(target.id));
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
      // Catalogue retire: `environment_id`, not `id`. The backend cascades to
      // every enrollment, setting `retired_at` and clearing both flags — apply
      // exactly that locally to this app's row rather than refetching, for the
      // same reason `promote` does.
      const retiredEnv = await retireProjectEnvironment(target.environment_id);
      envs = envs.map((e) =>
        e.environment_id === retiredEnv.id
          ? {
              ...e,
              name: retiredEnv.name,
              retired_at: retiredEnv.retired_at,
              ingest_enabled: false,
              is_default: false,
              updated_at: retiredEnv.updated_at,
            }
          : e,
      );
      confirmRetire = null;
      toastStore.success(`"${target.name}" retired project-wide. Its data stays queryable.`);
    } catch (err) {
      // A race with another admin can still trip the backend's "last live" or
      // "still some app's default" 409 even though we hide the button for those
      // cases locally — and the backend's default check spans every app in the
      // project, which this card cannot see. Surface it rather than failing
      // silently.
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }
</script>

<Card title="Environments">
  {#snippet actions()}
    <Button
      variant="secondary"
      size="sm"
      title="Adds the environment to every app in this project"
      lockedReason={createLock}
      onclick={() => (creating = true)}
    >
      New environment
    </Button>
  {/snippet}

  <p class="scope-note muted">
    Environments are defined by the project and shared by every app in it, so
    <strong>creating, renaming or retiring one changes it for all of them</strong>. The ingest
    key, the mute switch and the default below belong to this app alone.
  </p>

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
            <Button
              variant="ghost"
              size="sm"
              disabled={busyId === env.id}
              lockedReason={renameLock}
              title="Renames this environment for every app in the project"
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
              lockedReason={updateLock}
              onclick={() => toggleIngest(env)}
            >
              {env.ingest_enabled ? 'Mute ingest' : 'Resume ingest'}
            </Button>
            {#if !env.is_default}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                lockedReason={updateLock}
                onclick={() => promote(env)}
              >
                Make default
              </Button>
            {/if}
            <Button
              variant="ghost"
              size="sm"
              disabled={busyId === env.id}
              lockedReason={rotateLock}
              onclick={() => (confirmRotate = env)}
            >
              Rotate key
            </Button>
            <!-- `is_default` / `active.length` are business rules, not
                 permissions: an environment that cannot be retired by anyone
                 should not render a locked button implying a missing grant. -->
            {#if !env.is_default && active.length > 1}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                lockedReason={retireLock}
                title="Retires this environment for every app in the project"
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

<Modal bind:open={creating} title="New project environment" size="sm">
  <Input
    label="Name"
    bind:value={newName}
    placeholder="staging"
    hint="Added to every app in this project, each with its own ingest key. Lowercase and short works best — this appears in every filter."
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
  <Input
    label="Name"
    bind:value={renameValue}
    hint="This name belongs to the project — renaming it renames it for every app in the project, not just this one."
  />
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
  title="Retire environment for the whole project?"
  message={`"${confirmRetire?.name ?? ''}" is a project environment, so retiring it removes it from EVERY app in this project — not just this one. All of their keys for it stop working immediately and it leaves the picker. Existing data stays queryable and is archived to cold storage on the normal schedule. This cannot be undone.`}
  confirmLabel="Retire"
  danger
  loading={busyId !== null && busyId === confirmRetire?.id}
  onconfirm={doRetire}
  oncancel={() => (confirmRetire = null)}
/>

<style>
  .scope-note {
    font-size: 0.8rem;
    line-height: 1.55;
    margin: 0 0 0.85rem;
  }
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
