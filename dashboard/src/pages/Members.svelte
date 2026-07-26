<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import RoleEditorDialog from '../lib/components/members/RoleEditorDialog.svelte';
  import CreateMemberDialog from '../lib/components/members/CreateMemberDialog.svelte';
  import EditMemberDialog from '../lib/components/members/EditMemberDialog.svelte';
  import MembersTable from '../lib/components/members/MembersTable.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listMembers, listRoles, createGrant, deleteGrant, setMemberActive } from '../lib/api/orgs';
  import { listApps } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { groupMembers, type App, type Member, type MemberGrant, type Role, type ScopeOption } from '../lib/models';

  let members = $state<MemberGrant[]>([]);
  let roles = $state<Role[]>([]);
  let appsById = $state<Record<string, App>>({});
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Grant access form — grants a scoped role to someone who already has an
  // account (possibly in another org). This is distinct from "Create member"
  // below, which provisions a brand-new account. There is no invitation flow.
  let grantEmail = $state('');
  let grantRoleId = $state('');
  let grantScopeKey = $state(''); // `${scope_type}:${scope_id}`
  let granting = $state(false);
  let removingId = $state<string | null>(null);

  // Role editor dialog (create + edit + read-only view of system presets)
  let roleDialogOpen = $state(false);
  let editingRole = $state<Role | null>(null);

  // Create / edit member dialogs.
  let createOpen = $state(false);
  let editingMemberId = $state<string | null>(null);
  let togglingUserId = $state<string | null>(null);
  let deactivateTarget = $state<Member | null>(null);

  // One row per person: this recomputes fresh Member/grant objects every time
  // `members` is reloaded, so anything derived from it (below) is never a
  // stale reference held across a save.
  const grouped = $derived(groupMembers(members));

  // Re-derived (not captured at click time) so that after a save triggers
  // load() -> members update -> grouped recomputes, EditMemberDialog receives
  // a brand-new Member object and its internal dirty-tracking clears.
  const editingMember = $derived(
    editingMemberId ? (grouped.find((m) => m.user_id === editingMemberId) ?? null) : null,
  );

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

  const canManage = $derived(sessionStore.can('member:manage'));
  const canReadMembers = $derived(sessionStore.can('member:read'));
  const canManageRoles = $derived(sessionStore.can('role:manage'));

  const scopeOptions = $derived.by<ScopeOption[]>(() => {
    const opts: ScopeOption[] = [];
    const org = sessionStore.currentOrg;
    if (org) {
      opts.push({ key: `org:${org.id}`, label: `Org: ${org.name}`, scope_type: 'org', scope_id: org.id });
    }
    for (const p of sessionStore.projects) {
      opts.push({
        key: `project:${p.id}`,
        label: `Project: ${p.name}`,
        scope_type: 'project',
        scope_id: p.id,
      });
    }
    for (const a of Object.values(appsById)) {
      opts.push({ key: `app:${a.id}`, label: `App: ${a.name}`, scope_type: 'app', scope_id: a.id });
    }
    return opts;
  });

  async function load(orgId: string) {
    loading = true;
    error = null;
    try {
      const [mem, rls] = await Promise.all([listMembers(orgId), listRoles(orgId)]);
      members = mem;
      roles = rls;
      if (rls.length && !grantRoleId) grantRoleId = rls[0].id;
      // Resolve app names across every project so app-scoped grants read nicely.
      const appLists = await Promise.all(
        sessionStore.projects.map((p) => listApps(p.id).catch(() => [] as App[])),
      );
      const map: Record<string, App> = {};
      for (const list of appLists) for (const a of list) map[a.id] = a;
      appsById = map;
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const org = sessionStore.currentOrgId;
    if (org && canReadMembers) void load(org);
    else if (org) loading = false;
  });

  async function submitGrant(event: SubmitEvent) {
    event.preventDefault();
    const org = sessionStore.currentOrgId;
    if (!org || granting || !grantEmail.trim() || !grantRoleId || !grantScopeKey) return;
    const opt = scopeOptions.find((o) => o.key === grantScopeKey);
    if (!opt) return;
    granting = true;
    try {
      await createGrant(org, {
        email: grantEmail.trim(),
        role_id: grantRoleId,
        scope_type: opt.scope_type,
        scope_id: opt.scope_id,
      });
      grantEmail = '';
      grantScopeKey = '';
      await load(org);
      toastStore.success('Access granted.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      granting = false;
    }
  }

  async function removeGrant(id: string) {
    if (removingId) return;
    removingId = id;
    try {
      await deleteGrant(id);
      members = members.filter((m) => m.id !== id);
      toastStore.success('Access removed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      removingId = null;
    }
  }

  function openNewRole() {
    editingRole = null;
    roleDialogOpen = true;
  }

  function openEditRole(role: Role) {
    editingRole = role;
    roleDialogOpen = true;
  }

  function onRoleSaved(saved: Role) {
    const i = roles.findIndex((r) => r.id === saved.id);
    if (i >= 0) roles[i] = saved;
    else roles = [...roles, saved];
    toastStore.success(`Role "${saved.name}" saved.`);
  }

  async function toggleActive(member: Member) {
    const org = sessionStore.currentOrg;
    if (!org) return;
    togglingUserId = member.user_id;
    try {
      await setMemberActive(org.id, member.user_id, !member.is_active);
      toastStore.success(
        member.is_active
          ? `${member.email} can no longer sign in.`
          : `${member.email} can sign in again.`,
      );
      await load(org.id);
    } catch (err) {
      // The backend's 409s carry the actionable text (last owner, cross-org,
      // self) — surface it verbatim rather than a generic failure.
      toastStore.error(errorMessage(err));
    } finally {
      togglingUserId = null;
    }
  }

  function requestToggle(member: Member) {
    // Deactivation signs the person out of every device, so it is confirmed.
    // Reactivation is reversible and low-stakes, so it fires immediately.
    if (member.is_active) {
      deactivateTarget = member;
    } else {
      void toggleActive(member);
    }
  }

  async function confirmDeactivate() {
    const member = deactivateTarget;
    deactivateTarget = null;
    if (member) await toggleActive(member);
  }
</script>

<AppShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">Members</h1>
      <p class="muted sub">People with access to {sessionStore.currentOrg?.name ?? 'this org'}.</p>
    </div>
  </div>

  {#if !canReadMembers}
    <Card>
      <EmptyState
        title="No access"
        description="You don't have permission to view members of this organization."
        icon="lock"
      />
    </Card>
  {:else if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <Card><p class="err-msg">{error}</p></Card>
  {:else}
    {#if canManage}
      <Card class="grant-card">
        {#snippet header()}
          <div class="grant-head">
            <div>
              <h3 class="card-title-inline">Grant access</h3>
              <p class="muted grant-sub">
                For someone who already has an account — in this org or another. To provision a
                brand-new account instead, use Create member.
              </p>
            </div>
            <Button variant="primary" onclick={() => (createOpen = true)}>Create member</Button>
          </div>
        {/snippet}
        <form class="grant-form" onsubmit={submitGrant}>
          <div class="gf-field">
            <Input label="Email" type="email" bind:value={grantEmail} placeholder="teammate@company.com" required />
          </div>
          <div class="gf-field">
            <span class="lbl">Role</span>
            <select class="sel" bind:value={grantRoleId} aria-label="Role">
              {#each roles as role (role.id)}
                <option value={role.id}>{role.name}</option>
              {/each}
            </select>
          </div>
          <div class="gf-field">
            <span class="lbl">Scope</span>
            <select class="sel" bind:value={grantScopeKey} aria-label="Scope">
              <option value="" disabled>Select scope…</option>
              {#each scopeOptions as opt (opt.key)}
                <option value={opt.key}>{opt.label}</option>
              {/each}
            </select>
          </div>
          <Button type="submit" variant="primary" loading={granting}>Grant</Button>
        </form>
      </Card>
    {/if}

    <MembersTable
      {grouped}
      {appsById}
      {canManage}
      {removingId}
      {togglingUserId}
      onedit={(userId) => (editingMemberId = userId)}
      ontoggle={requestToggle}
      onremovegrant={removeGrant}
    />

    <Card class="roles-card">
      {#snippet header()}
        <div class="roles-head">
          <h3 class="card-title-inline">Roles</h3>
          {#if canManageRoles}
            <Button variant="secondary" size="sm" onclick={openNewRole}>New role</Button>
          {/if}
        </div>
      {/snippet}

      <ul class="role-list">
        {#each roles as role (role.id)}
          <li class="role-row">
            <div class="r-main">
              <span class="r-name">{role.name}</span>
              {#if role.is_system}<Badge tone="neutral" size="sm">system</Badge>{/if}
              {#if role.description}<span class="r-desc muted">{role.description}</span>{/if}
            </div>
            <div class="r-actions">
              <span class="r-count muted">{role.permissions.length} permissions</span>
              <Button
                variant="ghost"
                size="sm"
                onclick={() => openEditRole(role)}
                disabled={!canManageRoles && !role.is_system}
              >
                {role.is_system ? 'View' : 'Edit'}
              </Button>
            </div>
          </li>
        {/each}
      </ul>
    </Card>
  {/if}

  {#if sessionStore.currentOrg}
    <RoleEditorDialog
      open={roleDialogOpen}
      orgId={sessionStore.currentOrg.id}
      role={editingRole}
      memberCount={editingRole ? (roleMemberCounts[editingRole.id] ?? 0) : 0}
      onclose={() => (roleDialogOpen = false)}
      onsaved={onRoleSaved}
    />
    <CreateMemberDialog
      open={createOpen}
      orgId={sessionStore.currentOrg.id}
      {roles}
      {scopeOptions}
      onclose={() => (createOpen = false)}
      oncreated={() => load(sessionStore.currentOrg!.id)}
    />
    <EditMemberDialog
      open={editingMemberId !== null}
      orgId={sessionStore.currentOrg.id}
      member={editingMember}
      {roles}
      {scopeOptions}
      onclose={() => (editingMemberId = null)}
      onchanged={() => load(sessionStore.currentOrg!.id)}
    />
  {/if}

  {#if deactivateTarget}
    <ConfirmDialog
      open
      title="Deactivate member?"
      message={`${deactivateTarget.email} will be signed out of every device and won't be able to sign in until reactivated. Their access grants are kept.`}
      confirmLabel="Deactivate"
      danger
      onconfirm={confirmDeactivate}
      oncancel={() => (deactivateTarget = null)}
    />
  {/if}
</AppShell>

<style>
  .head {
    margin-bottom: 18px;
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
  :global(.grant-card),
  :global(.roles-card) {
    margin-bottom: 16px;
  }
  .grant-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    width: 100%;
    gap: 16px;
  }
  .grant-sub {
    font-size: 12.5px;
    margin-top: 3px;
    max-width: 46ch;
  }
  .grant-form {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    flex-wrap: wrap;
  }
  .gf-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 180px;
    flex: 1;
  }
  .lbl {
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
  }
  .sel {
    padding: 10px 13px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-size: 13.5px;
    outline: none;
    height: 40px;
  }
  .sel option {
    background: var(--surface);
    color: var(--text);
  }
  .card-title-inline {
    font-size: 14.5px;
    font-weight: 620;
  }
  .roles-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    gap: 12px;
  }
  .role-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .role-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 11px 0;
    border-bottom: 1px solid var(--border);
  }
  .role-row:last-child {
    border-bottom: none;
  }
  .r-main {
    display: flex;
    align-items: center;
    gap: 9px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .r-name {
    font-weight: 600;
    font-size: 13.5px;
  }
  .r-desc {
    font-size: 12.5px;
  }
  .r-count {
    font-size: 12px;
    white-space: nowrap;
  }
  .r-actions {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-shrink: 0;
  }
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }
</style>
