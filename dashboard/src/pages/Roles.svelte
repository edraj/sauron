<script lang="ts">
  import { t } from '../lib/i18n';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import RowActionsMenu from '../lib/components/ui/RowActionsMenu.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import RoleEditorDialog from '../lib/components/members/RoleEditorDialog.svelte';
  import DeleteRoleDialog from '../lib/components/members/DeleteRoleDialog.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
  import { lockTip } from '../lib/actions/lock-tip';
  import { listRoles, listMembers } from '../lib/api/orgs';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { ROLE_DEFAULT_SORT, roleAccessor } from '../lib/models/role-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import { groupMembers, type MemberGrant, type Permission, type Role } from '../lib/models';

  // Cached view (lib/stores/cached-view.svelte.ts): the catalogue paints
  // instantly on a revisit, then refreshes behind the existing rows.
  //
  // ONE view holding both responses rather than two, because the two are only
  // ever useful together — `roleMemberCounts` derives from `members` and is
  // read by the roles table's own sort, so a cache that could serve fresh roles
  // beside stale members would order the column by a different number than the
  // cell displays.
  //
  // `members` is loaded purely to feed `roleMemberCounts`; this page has no
  // member-management UI of its own.
  const view = new CachedView<{ roles: Role[]; members: MemberGrant[] }>();

  const roles = $derived(view.data?.roles ?? []);
  const members = $derived(view.data?.members ?? []);
  const loading = $derived(view.loading);
  const error = $derived(view.error);

  // Role editor dialog (create + edit + read-only view of system presets).
  // Exactly one of `editingRole` / `copyFromRole` is set at a time — `open*`
  // below always resets the other — since `RoleEditorDialog` treats a set
  // `role` (edit/view) as taking precedence over `copyFrom` (create,
  // prefilled).
  let roleDialogOpen = $state(false);
  let editingRole = $state<Role | null>(null);
  let copyFromRole = $state<Role | null>(null);

  // Delete-role confirmation dialog. Custom roles only — system presets never
  // get a Delete item (see the `is_system` guard on the menu item below).
  let deleteDialogOpen = $state(false);
  let deletingRole = $state<Role | null>(null);

  // One row per person: this recomputes fresh Member/grant objects every time
  // `members` is reloaded, so anything derived from it (below) is never a
  // stale reference held across a save.
  const grouped = $derived(groupMembers(members));

  // Distinct users per role, not grant count — a person holding the same role
  // at three scopes must still only count once, since that's what the
  // "N members hold this role" impact warning in RoleEditorDialog means.
  const roleMemberCounts = $derived.by(() => {
    const usersByRole = new Map<string, Set<string>>();
    for (const m of grouped) {
      for (const g of m.grants) {
        let users = usersByRole.get(g.role_id);
        if (!users) {
          users = new Set();
          usersByRole.set(g.role_id, users);
        }
        users.add(m.user_id);
      }
    }
    const counts: Record<string, number> = {};
    for (const [roleId, users] of usersByRole) counts[roleId] = users.size;
    return counts;
  });

  // `list_roles` returns the org's whole catalogue in one response — a handful
  // of rows — so the sort runs over the ENTIRE array and there is no pager: a
  // pager on a five-row table implies a page two that does not exist.
  //
  // A bare `SortState`, not the `OffsetListState` the paginated tables use:
  // that type exists to make "apply a sort" and "reset to page 1" one
  // indivisible step, and with no offset there is nothing to reset. Its
  // `key`/`dir` are `readonly` (see `sort.ts`), so `sort.dir = 'asc'` is a
  // type error and every transition goes through `toggleSort`.
  let sort = $state<SortState>(ROLE_DEFAULT_SORT);

  // `sortRows` copies before sorting, so `roles` — the array `onRoleSaved` and
  // `onRoleDeleted` write back into by identity — is never reordered in place
  // underneath them.
  //
  // `roleMemberCounts` is passed in rather than restated inside the accessor
  // module: it is derived from the members list, and there must be exactly one
  // definition of "how many people hold this role" or the column orders by a
  // different number than the cell displays.
  const sortedRoles = $derived(
    sortRows(roles, roleAccessor(sort.key, roleMemberCounts), sort.dir),
  );

  function onsort(key: string, columnDefault: SortDir) {
    sort = toggleSort(sort, key, columnDefault);
  }

  // orgs.rs:1399,1454,1538 resolve create/update/delete through authorize_org
  // with ROLE_MANAGE — no project- or app-scoped grant can satisfy it, hence
  // `level: 'org'`. Reading the catalogue (list_roles, orgs.rs:1380) needs only
  // `member:read`, which is what PAGE_ACCESS gates this route on already.
  const roleManageLock = $derived(lockedBy('role:manage', { level: 'org' }));

  // Shared by the Delete AND Copy row actions — both funnel through server
  // checks that require the exact same two things: `role:manage`, and, at org
  // scope, every permission the role currently confers.
  //   - Delete is treated as an edit to the empty permission set
  //     (`check_role_edit`, orgs.rs:1564): the symmetric difference of the
  //     role's permissions against `[]` is the role's own permissions, so
  //     DELETE requires holding all of them.
  //   - Copy opens the CREATE path prefilled from the source role (see
  //     `copyFrom` on RoleEditorDialog). `create_role` (orgs.rs:1399) is
  //     gated on ROLE_MANAGE, and its no-escalation check (orgs.rs:1409-1416)
  //     rejects any permission the creator does not hold at org scope — the
  //     same "every permission the role confers" requirement.
  // An Admin (role:manage, no org:manage) can delete/copy a Developer-shaped
  // role but not one that grants org:manage. `roleManageLock` alone would show
  // an enabled control that 403s for exactly the case this guard exists to
  // stop, so this short-circuits on it first, then walks the role's
  // permissions the same way the server does and returns the first one the
  // caller does not hold at org scope — the actual blocking permission, named
  // in the tooltip via `lockTitle`, or `null` once every permission clears.
  function blockedByMissingPermission(role: Role): Permission | null {
    if (roleManageLock) return roleManageLock;
    for (const p of role.permissions) {
      if (!sessionStore.can(p, { level: 'org' })) return p;
    }
    return null;
  }

  /**
   * `force` bypasses the fresh-window short-circuit. Every mutation on this page
   * goes through it, because a re-list that joined a flight issued before the
   * write would return the pre-write catalogue and `set` would then cache it —
   * the deleted role reappears and stays for the whole fresh window.
   */
  async function load(orgId: string, force = false) {
    await view.load(
      viewKey('roles.list', orgId),
      async () => {
        const [roles, members] = await Promise.all([listRoles(orgId), listMembers(orgId)]);
        return { roles, members };
      },
      force,
    );
  }

  /**
   * Re-list after a create/edit/delete instead of splicing the row into `roles`
   * locally.
   *
   * The local edit is what the old code did, and it cannot survive caching:
   * `view.data` is the VERY object the cache holds, handed back by reference,
   * so `roles[i] = saved` would write through into the cached payload and the
   * edit would persist across navigations even if the server had rejected it.
   * A forced re-list is one request and cannot drift from what the server
   * actually stored.
   */
  async function reload() {
    const org = sessionStore.currentOrgId;
    if (!org) return;
    viewCache.invalidate('roles.list');
    await load(org, true);
  }

  function openNewRole() {
    editingRole = null;
    copyFromRole = null;
    roleDialogOpen = true;
  }

  function openEditRole(role: Role) {
    editingRole = role;
    copyFromRole = null;
    roleDialogOpen = true;
  }

  function openCopyRole(role: Role) {
    editingRole = null;
    copyFromRole = role;
    roleDialogOpen = true;
  }

  async function onRoleSaved(saved: Role) {
    toastStore.success(`Role "${saved.name}" saved.`);
    await reload();
  }

  function openDeleteRole(role: Role) {
    deletingRole = role;
    deleteDialogOpen = true;
  }

  async function onRoleDeleted(role: Role, revokedGrants: number) {
    await reload();
    toastStore.success(
      revokedGrants > 0
        ? `Deleted "${role.name}" and revoked ${revokedGrants} grant${revokedGrants === 1 ? '' : 's'}.`
        : `Deleted "${role.name}".`,
    );
  }

  $effect(() => {
    const org = sessionStore.currentOrgId;
    if (org) void load(org);
  });
</script>

<AdminShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">{t('roles.title')}</h1>
      <p class="muted sub">
        Permission sets that can be granted to members of {sessionStore.currentOrg?.name ?? 'this org'}.
      </p>
    </div>
    <Button variant="primary" lockedReason={roleManageLock} onclick={openNewRole}>
      {t('roles.new')}
    </Button>
  </div>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <Card><p class="err-msg">{error}</p></Card>
  {:else if roles.length === 0}
    <EmptyState
      title={t('roles.empty.title')}
      description={t('roles.empty.body')}
      icon="shield-check"
    >
      {#snippet action()}
        <Button variant="primary" lockedReason={roleManageLock} onclick={openNewRole}>
          {t('roles.new')}
        </Button>
      {/snippet}
    </EmptyState>
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <SortableTh key="name" columnDefault="asc" {sort} {onsort}>{t('common.name')}</SortableTh>
          <SortableTh key="description" columnDefault="asc" {sort} {onsort}>{t('roles.column.description')}</SortableTh>
          <!-- By COUNT, which is what the cell renders. A header that ordered
               by "whichever permission comes first alphabetically" would be
               worse than one that does not sort. -->
          <SortableTh key="permissions" class="num" {sort} {onsort}>{t('roles.column.permissions')}</SortableTh>
          <SortableTh key="members" class="num" {sort} {onsort}>{t('members.title')}</SortableTh>
          <!-- The row actions menu. Nothing to order by. -->
          <th class="col-act"></th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each sortedRoles as role (role.id)}
          {@const rowLock = role.is_system ? null : roleManageLock}
          {@const copyLock = blockedByMissingPermission(role)}
          {@const deleteLock = role.is_system ? null : blockedByMissingPermission(role)}
          <tr>
            <td>
              <div class="name-cell">
                <span class="name">{role.name}</span>
                {#if role.is_system}<Badge tone="neutral" size="sm">system</Badge>{/if}
              </div>
            </td>
            <td class="wrap">
              {#if role.description}<span class="cell-muted">{role.description}</span>{:else}<span class="faint">—</span>{/if}
            </td>
            <td class="num">{role.permissions.length}</td>
            <td class="num">{roleMemberCounts[role.id] ?? 0}</td>
            <td class="col-act">
              <RowActionsMenu label={`Actions for ${role.name}`}>
                {#snippet children(close)}
                  <!-- A system role opens read-only for anyone, so it is never
                       locked; only editing a custom role needs `role:manage`. -->
                  <button
                    type="button"
                    role="menuitem"
                    class="ram-item"
                    use:lockTip={rowLock}
                    onclick={() => {
                      close();
                      openEditRole(role);
                    }}
                  >
                    {#if rowLock}<span class="ram-lock" aria-hidden="true"
                        ><Icon name="lock" size={12} /></span
                      >{/if}{role.is_system ? 'View' : 'Edit'}
                  </button>
                  <!-- Offered on system presets too — copying Developer or
                       Owner into an editable starting point is the primary
                       use case. Presets are read-only to edit, not to read. -->
                  <button
                    type="button"
                    role="menuitem"
                    class="ram-item"
                    use:lockTip={copyLock}
                    onclick={() => {
                      close();
                      openCopyRole(role);
                    }}
                  >
                    {#if copyLock}<span class="ram-lock" aria-hidden="true"
                        ><Icon name="lock" size={12} /></span
                      >{/if}Copy
                  </button>
                  {#if !role.is_system}
                    <!-- System presets get no Delete item at all — the server
                         returns 400 for them (orgs.rs:1545), and a control that
                         can only ever fail is worse than omitting it. -->
                    <button
                      type="button"
                      role="menuitem"
                      class="ram-item danger"
                      use:lockTip={deleteLock}
                      onclick={() => {
                        close();
                        openDeleteRole(role);
                      }}
                    >
                      {#if deleteLock}<span class="ram-lock" aria-hidden="true"
                          ><Icon name="lock" size={12} /></span
                        >{/if}Delete
                    </button>
                  {/if}
                {/snippet}
              </RowActionsMenu>
            </td>
          </tr>
        {/each}
      {/snippet}
    </DataTable>
  {/if}

  {#if sessionStore.currentOrg}
    <RoleEditorDialog
      open={roleDialogOpen}
      orgId={sessionStore.currentOrg.id}
      role={editingRole}
      copyFrom={copyFromRole}
      memberCount={editingRole ? (roleMemberCounts[editingRole.id] ?? 0) : 0}
      onclose={() => (roleDialogOpen = false)}
      onsaved={onRoleSaved}
    />
    <DeleteRoleDialog
      open={deleteDialogOpen}
      orgId={sessionStore.currentOrg.id}
      role={deletingRole}
      memberCount={deletingRole ? (roleMemberCounts[deletingRole.id] ?? 0) : 0}
      onclose={() => (deleteDialogOpen = false)}
      ondeleted={onRoleDeleted}
    />
  {/if}
</AdminShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }
  .name-cell {
    display: inline-flex;
    align-items: center;
    gap: 9px;
  }
  .name {
    font-weight: 600;
  }
  .col-act {
    text-align: end;
    width: 1%;
    white-space: nowrap;
  }
</style>
