<script lang="ts">
  import { formatNumber, labelFor, t, type MessageKey } from '../../i18n';
  import { errorMessage } from '../../api/client';
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Skeleton from '../ui/Skeleton.svelte';
  import EmptyState from '../ui/EmptyState.svelte';
  import ErrorState from '../ui/ErrorState.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import DataTable from '../DataTable.svelte';
  import SortableTh from '../SortableTh.svelte';
  import ClientPager from '../ClientPager.svelte';
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
  import { setOffsetPage, setOffsetSort, type OffsetListState } from '../../models/list-state';
  import {
    SUBSCRIPTION_DEFAULT_SORT,
    subscriptionAccessor,
  } from '../../models/subscription-sort';
  import { pageSlice } from '../../models/paginate';
  import { sortRows } from '../../models/sort-rows';
  import type { SortDir } from '../../models/sort';
  import type {
    NotificationSubscription,
    SubscriptionDelivery,
    SubscriptionKind,
  } from '../../models';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';

  const KIND_LABELS: Record<string, MessageKey> = {
    uptime: 'notif.kind.uptime',
    error_spike: 'notif.kind.errorSpike',
    error_new_issue: 'notif.kind.newIssue',
    error_regression: 'notif.kind.regression',
  };

  // Named once so the "Notify about" column can be SORTED by exactly the text
  // it renders; handing the accessor a lookup that disagreed with the cell is
  // the "orders by one value, displays another" bug.
  const kindLabel = (k: SubscriptionKind): string => labelFor(KIND_LABELS, k);

  const DELIVERY_LABELS: Record<SubscriptionDelivery, MessageKey> = {
    immediate: 'notif.dialog.asItHappens',
    hourly: 'notif.dialog.hourly',
    daily: 'notif.dialog.daily',
  };
  const deliveryLabel = (d: SubscriptionDelivery): string => labelFor(DELIVERY_LABELS, d);

  /** Rows per page. The list arrives whole, so this is a rendering budget only. */
  const PAGE = 25;

  let subs = $state<NotificationSubscription[]>([]);
  let loading = $state(true);
  // Reads set a local error (the caught error itself, so ErrorState can say
  // what kind of failure it was); mutations toast.
  let loadError = $state<unknown>(null);
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
  const orgName = $derived(sessionStore.currentOrg?.name ?? t('members.thisOrganization'));

  // `/v1/me/notification-subscriptions` returns every subscription in one
  // response, so the sort and the pager both run here, over the SAME array:
  // order the whole list first, then take a window out of it. Sorting the
  // window instead would reorder only what is on screen while presenting
  // itself as having ordered everything.
  //
  // Sort and offset are one `OffsetListState` rather than two variables because
  // `setOffsetSort` resets the offset as part of applying a sort.
  let list = $state<OffsetListState>({ sort: SUBSCRIPTION_DEFAULT_SORT, offset: 0 });

  // `sortRows` copies before sorting. `subs` is replaced wholesale by `load()`
  // and read by the dialog through `editing`, so an in-place sort would reorder
  // the array every other reader holds.
  const sorted = $derived(
    sortRows(subs, subscriptionAccessor(list.sort.key, kindLabel), list.sort.dir),
  );
  const page = $derived(pageSlice(sorted, list.offset, PAGE));

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  async function load() {
    loading = true;
    loadError = null;
    try {
      subs = await listSubscriptions();
      projects = (await listProjects(orgId)).map((p) => ({ id: p.id, name: p.name }));
      // Apps are loaded per project up front here (a personal account has far
      // fewer projects than an org member admin screen), but environments stay
      // on-demand: there is no batched org-wide environments endpoint.
      //
      // Concurrent, not a serial `for…await` — the requests are independent, so
      // the old loop made load time the SUM of every project's round trip
      // instead of the slowest one. `Promise.all` also keeps the previous
      // failure behaviour exactly: the first rejection propagates to the
      // `catch` below and becomes `loadError`, just as an awaited throw
      // mid-loop did.
      const perProject = await Promise.all(
        projects.map(async (p) => {
          const apps = await listApps(p.id);
          return [p.id, apps.map((a) => ({ id: a.id, name: a.name }))] as const;
        }),
      );
      appsByProject = Object.fromEntries(perProject);
    } catch (e) {
      loadError = e;
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
      toastStore.success(s.enabled ? t('notif.toast.disabled') : t('notif.toast.enabled'));
      await load();
    } catch (e) {
      toastStore.error(errorMessage(e, t('notif.toast.updateFailed')));
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
      toastStore.success(t('notif.toast.deleted'));
      confirming = null;
      await load();
    } catch (e) {
      toastStore.error(errorMessage(e, t('notif.toast.deleteFailed')));
    } finally {
      busyId = '';
    }
  }

  $effect(() => {
    if (orgId) void load();
  });
</script>

<Card title={t('notif.title')}>
  {#snippet actions()}
    <Button
      variant="primary"
      size="sm"
      onclick={() => {
        editing = null;
        dialogOpen = true;
      }}
    >{t('notif.new')}</Button>
  {/snippet}

  {#if loading}
    <Skeleton rows={4} />
  {:else if loadError}
    <ErrorState
      size="section"
      title={t('common.couldntLoad', { thing: t('notif.thing') })}
      error={loadError}
      onretry={() => void load()}
    />
  {:else if subs.length === 0}
    <EmptyState
      size="section"
      icon="bell"
      title={t('notif.empty.title')}
      description={t('notif.empty.body')}
    />
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <SortableTh key="scope" columnDefault="asc" sort={list.sort} {onsort}>{t('notif.dialog.scope')}</SortableTh>
          <SortableTh key="kind" columnDefault="asc" sort={list.sort} {onsort}>
            {t('notif.column.notifyAbout')}
          </SortableTh>
          <SortableTh key="environments" columnDefault="asc" sort={list.sort} {onsort}>
            {t('notif.column.environments')}
          </SortableTh>
          <SortableTh key="delivery" columnDefault="asc" sort={list.sort} {onsort}>
            {t('notif.column.delivery')}
          </SortableTh>
          <SortableTh key="quiet_hours" columnDefault="asc" sort={list.sort} {onsort}>
            {t('notif.column.quietHours')}
          </SortableTh>
          <SortableTh key="state" columnDefault="asc" sort={list.sort} {onsort}>{t('notif.column.state')}</SortableTh>
          <!-- Edit / Enable / Delete: no value to order by. -->
          <th></th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each page.rows as s (s.id)}
          <tr>
            <td>{describeSubscription(s)}</td>
            <td>{kindLabel(s.kind)}</td>
            <td>
              {s.environment_ids.length === 0
                ? t('common.all')
                : formatNumber(s.environment_ids.length)}
            </td>
            <td>
              {deliveryLabel(s.effective_delivery)}
              {#if s.effective_delivery !== s.delivery}
                <Badge tone="warning" size="sm">{t('notif.capped')}</Badge>
              {/if}
            </td>
            <td>{quietHoursLabel(s.quiet_start_min, s.quiet_end_min, s.quiet_tz)}</td>
            <td>
              {#if s.enabled}
                <Badge tone="success" size="sm">{t('notif.state.on')}</Badge>
              {:else if s.disabled_reason === 'access_revoked'}
                <!-- Explain rather than look broken: the subscription is off
                     because the owner lost access to its scope, and re-granting
                     access deliberately does not resurrect it. -->
                <Badge tone="warning" size="sm">{t('notif.state.offAccessRemoved')}</Badge>
              {:else}
                <Badge tone="neutral" size="sm">{t('notif.state.off')}</Badge>
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
              >{t('common.edit')}</Button>
              <Button size="sm" disabled={busyId === s.id} onclick={() => toggle(s)}>
                {s.enabled ? t('alerts.disable') : t('alerts.enable')}
              </Button>
              <Button size="sm" variant="danger" onclick={() => (confirming = s)}>{t('common.delete')}</Button>
            </td>
          </tr>
        {/each}
      {/snippet}
    </DataTable>

    <!-- `total` is the length of the EXACT array handed to `pageSlice` above —
         `sorted`, the same expression, not "all the subscriptions". A pager
         measuring a longer list than the one being sliced re-creates the
         enabled-Next-onto-an-empty-page bug that `Pagination.hasNext` was made
         a required prop to kill. (Nothing filters this table. If anything ever
         does, the filter must both feed `pageSlice` and be measured here, AND
         reset the offset with `setOffsetPage(list, 0)`.) -->
    <ClientPager
      offset={list.offset}
      limit={PAGE}
      total={sorted.length}
      onchange={(o) => (list = setOffsetPage(list, o))}
    />
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
  title={t('notif.delete.title')}
  message={t('notif.delete.body')}
  confirmLabel={t('common.delete')}
  danger
  loading={busyId === confirming?.id}
  onconfirm={remove}
  oncancel={() => (confirming = null)}
/>

<style>
  .acts { display: flex; gap: 6px; justify-content: flex-end; }
</style>
