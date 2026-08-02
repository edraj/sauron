<script lang="ts">
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import EmptyState from '../ui/EmptyState.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import DataTable from '../DataTable.svelte';
  import SubscriptionDialog from './SubscriptionDialog.svelte';
  import {
    deleteSubscription,
    listSubscriptions,
    updateSubscription,
  } from '../../api/notification-prefs';
  import { listProjects } from '../../api/projects';
  import { listApps } from '../../api/apps';
  import { listEnvironments, listProjectEnvironments } from '../../api/environments';
  import { describeSubscription, quietHoursLabel } from '../../models/notification-prefs';
  import type { NotificationSubscription } from '../../models';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';

  const KIND_LABELS: Record<string, string> = {
    uptime: 'Uptime',
    error_spike: 'Error rate increasing',
    error_new_issue: 'New issue',
    error_regression: 'Issue regressed',
  };

  let subs = $state<NotificationSubscription[]>([]);
  let loading = $state(true);
  // Reads set a local error; mutations toast. Both conventions in one file.
  let loadError = $state('');
  let projects = $state<{ id: string; name: string }[]>([]);
  let appsByProject = $state<Record<string, { id: string; name: string }[]>>({});
  let catalogueEnvsByProject = $state<Record<string, { id: string; name: string }[]>>({});
  let dialogOpen = $state(false);
  let editing = $state<NotificationSubscription | null>(null);
  let confirming = $state<NotificationSubscription | null>(null);
  let busyId = $state('');

  // `authStore` carries authentication, not the org selection — the current org
  // lives on `sessionStore`, which is where every other page reads it from
  // (`pages/Members.svelte:385`).
  const orgId = $derived(sessionStore.currentOrg?.id ?? '');
  const orgName = $derived(sessionStore.currentOrg?.name ?? 'Organization');

  async function load() {
    loading = true;
    loadError = '';
    try {
      subs = await listSubscriptions();
      projects = (await listProjects(orgId)).map((p) => ({ id: p.id, name: p.name }));
      // Apps are loaded per project up front here (a personal account has far
      // fewer projects than an org member admin screen), but environments stay
      // on-demand: there is no batched org-wide environments endpoint.
      const next: Record<string, { id: string; name: string }[]> = {};
      for (const p of projects) {
        next[p.id] = (await listApps(p.id)).map((a) => ({ id: a.id, name: a.name }));
      }
      appsByProject = next;
    } catch (e) {
      loadError = e instanceof Error ? e.message : 'Could not load subscriptions';
    } finally {
      loading = false;
    }
  }

  async function loadEnvs(projectId: string) {
    if (catalogueEnvsByProject[projectId]) return;
    let envs: { id: string; name: string }[] = [];
    try {
      envs = (await listProjectEnvironments(projectId)).map((e) => ({ id: e.id, name: e.name }));
    } catch {
      // `GET /v1/projects/{id}/environments` is project-authorized, so it 403s
      // for an app-scoped member — who is precisely the member environment
      // narrowing exists for. Leaving the row empty would make the env chips
      // unreachable from the UI for exactly the users `covers()` arm 5 was
      // written to serve. `GET /v1/apps/{app_id}/environments` is `reach_for`-
      // based, so fall back to it and rebuild the CATALOGUE list from each
      // enrollment's `environment_id`.
      //
      // The chip value must stay a CATALOGUE id: the create/patch endpoints
      // validate `environment_ids` against the project's live catalogue and
      // reject an enrollment id outright. `AppEnvironment.id` is the enrollment
      // id and is the wrong one to send.
      const byId = new Map<string, string>();
      for (const app of appsByProject[projectId] ?? []) {
        try {
          for (const e of await listEnvironments(app.id)) {
            byId.set(e.environment_id, e.name);
          }
        } catch {
          // One unreachable app must not blank the whole picker: an app-scoped
          // member reaches some apps of this project and not others.
        }
      }
      envs = [...byId].map(([id, name]) => ({ id, name }));
    }
    // Replaced, never mutated: a Record inside `$state` is a proxy and an
    // in-place assignment does not reliably re-derive downstream.
    catalogueEnvsByProject = { ...catalogueEnvsByProject, [projectId]: envs };
  }

  async function toggle(s: NotificationSubscription) {
    busyId = s.id;
    try {
      await updateSubscription(s.id, { enabled: !s.enabled });
      toastStore.success(s.enabled ? 'Subscription disabled' : 'Subscription enabled');
      await load();
    } catch (e) {
      toastStore.error(e instanceof Error ? e.message : 'Could not update the subscription');
    } finally {
      busyId = '';
    }
  }

  async function remove() {
    const s = confirming;
    if (!s) return;
    busyId = s.id;
    try {
      await deleteSubscription(s.id);
      toastStore.success('Subscription deleted');
      confirming = null;
      await load();
    } catch (e) {
      toastStore.error(e instanceof Error ? e.message : 'Could not delete the subscription');
    } finally {
      busyId = '';
    }
  }

  $effect(() => {
    if (orgId) void load();
  });
</script>

<Card title="Notifications">
  {#snippet actions()}
    <Button
      variant="primary"
      size="sm"
      onclick={() => {
        editing = null;
        dialogOpen = true;
      }}
    >New subscription</Button>
  {/snippet}

  {#if loading}
    <Spinner />
  {:else if loadError}
    <p class="err">{loadError}</p>
  {:else if subs.length === 0}
    <EmptyState
      icon="bell"
      title="No personal notifications yet"
      description="Subscribe yourself to uptime or error notifications for a project or app. Only you see and control these."
    />
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <th>Scope</th>
          <th>Notify about</th>
          <th>Environments</th>
          <th>Delivery</th>
          <th>Quiet hours</th>
          <th>State</th>
          <th></th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each subs as s (s.id)}
          <tr>
            <td>{describeSubscription(s)}</td>
            <td>{KIND_LABELS[s.kind] ?? s.kind}</td>
            <td>{s.environment_ids.length === 0 ? 'All' : s.environment_ids.length}</td>
            <td>
              {s.effective_delivery}
              {#if s.effective_delivery !== s.delivery}
                <Badge tone="warning" size="sm">capped</Badge>
              {/if}
            </td>
            <td>{quietHoursLabel(s.quiet_start_min, s.quiet_end_min, s.quiet_tz)}</td>
            <td>
              {#if s.enabled}
                <Badge tone="success" size="sm">On</Badge>
              {:else if s.disabled_reason === 'access_revoked'}
                <!-- Explain rather than look broken: the subscription is off
                     because the owner lost access to its scope, and re-granting
                     access deliberately does not resurrect it. -->
                <Badge tone="warning" size="sm">Off — access removed</Badge>
              {:else}
                <Badge tone="neutral" size="sm">Off</Badge>
              {/if}
            </td>
            <td class="acts">
              <Button
                size="sm"
                disabled={busyId === s.id}
                onclick={() => {
                  editing = s;
                  dialogOpen = true;
                }}
              >Edit</Button>
              <Button size="sm" disabled={busyId === s.id} onclick={() => toggle(s)}>
                {s.enabled ? 'Disable' : 'Enable'}
              </Button>
              <Button size="sm" variant="danger" onclick={() => (confirming = s)}>Delete</Button>
            </td>
          </tr>
        {/each}
      {/snippet}
    </DataTable>
  {/if}
</Card>

<SubscriptionDialog
  bind:open={dialogOpen}
  {orgId}
  {orgName}
  {projects}
  {appsByProject}
  {catalogueEnvsByProject}
  existing={editing}
  onopenproject={(id) => void loadEnvs(id)}
  onsaved={() => {
    dialogOpen = false;
    void load();
  }}
  onclose={() => (dialogOpen = false)}
/>

<ConfirmDialog
  open={confirming !== null}
  title="Delete subscription"
  message="You will stop receiving these notifications. This does not affect anyone else."
  confirmLabel="Delete"
  danger
  loading={busyId === confirming?.id}
  onconfirm={remove}
  oncancel={() => (confirming = null)}
/>

<style>
  /* --danger is not defined in app.css; the theme name is --error. An undefined
     custom property with no fallback invalidates the whole declaration, so the
     load-failure text would silently inherit body colour and stop reading as a
     failure. */
  .err { font-size: 13px; color: var(--error); margin: 0; }
  .acts { display: flex; gap: 6px; justify-content: flex-end; }
</style>
