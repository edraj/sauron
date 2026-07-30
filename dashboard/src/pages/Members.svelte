<script lang="ts">
  import { untrack } from 'svelte';
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
  import ScopeTree from '../lib/components/members/ScopeTree.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listMembers, listRoles, createGrant, deleteGrant, setMemberActive } from '../lib/api/orgs';
  import { listApps } from '../lib/api/apps';
  import { listEnvironments } from '../lib/api/environments';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    groupMembers,
    type App,
    type Environment,
    type Member,
    type MemberGrant,
    type Role,
  } from '../lib/models';
  import {
    EMPTY_SELECTION,
    isEmptySelection,
    selectionToScopes,
    type ScopeSelection,
  } from '../lib/models/scope-tree';

  let members = $state<MemberGrant[]>([]);
  let roles = $state<Role[]>([]);
  // Keyed by project because the scope tree renders by project; `appsById` is
  // the flattened view the table and the scope labels want.
  let appsByProject = $state<Record<string, App[]>>({});
  // The app load below is async and races the dialogs opening. Until it
  // settles, EditMemberDialog cannot tell an app-scoped grant from one whose
  // target it can't see, so it waits rather than seeding a wrong tree.
  let appsLoaded = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Environments per app, keyed by app id — the fourth scope level. Unlike
  // projects and apps this is NOT loaded eagerly: an org can have hundreds of
  // apps, and there is no batched "environments for this org" endpoint (only
  // GET /v1/apps/{app_id}/environments), so fetching every app's environments
  // up front would be exactly the N+1 the tree must not cause. Instead this
  // cache is filled lazily, one app at a time, the first time that app's row
  // is expanded anywhere the scope tree is rendered (grant form, create
  // dialog, edit dialog) — see ScopeTree's `onopenapp`. It is shared across
  // all three so expanding an app once benefits every surface for the rest of
  // the page session, including the members table's env-scope labels below.
  let envsByApp = $state<Record<string, Environment[]>>({});
  let loadingEnvApps = $state<Set<string>>(new Set());

  async function ensureEnvsLoaded(appId: string) {
    if (appId in envsByApp || loadingEnvApps.has(appId)) return;
    loadingEnvApps = new Set(loadingEnvApps).add(appId);
    try {
      const envs = await listEnvironments(appId);
      envsByApp = { ...envsByApp, [appId]: envs };
    } catch {
      // Left unloaded — the twisty simply retries the fetch next time that
      // app's row is expanded.
    } finally {
      const next = new Set(loadingEnvApps);
      next.delete(appId);
      loadingEnvApps = next;
    }
  }

  const appsById = $derived.by(() => {
    const map: Record<string, App> = {};
    for (const list of Object.values(appsByProject)) for (const a of list) map[a.id] = a;
    return map;
  });

  const projectsById = $derived.by(() => {
    const map: Record<string, { name: string }> = {};
    for (const p of sessionStore.projects) map[p.id] = { name: p.name };
    return map;
  });

  // Grant access form — grants a scoped role to someone who already has an
  // account (possibly in another org). This is distinct from "Create member"
  // below, which provisions a brand-new account. There is no invitation flow.
  let grantEmail = $state('');
  let grantRoleId = $state('');
  // Fresh arrays — EMPTY_SELECTION's are frozen, and $state proxies what it is
  // handed.
  let grantSelection = $state<ScopeSelection>({ ...EMPTY_SELECTION, projects: [], apps: [], envs: [] });
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

  const projectOfApp = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const [projectId, list] of Object.entries(appsByProject)) {
      for (const a of list) map[a.id] = projectId;
    }
    return map;
  });

  const appOfEnv = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const [appId, envs] of Object.entries(envsByApp)) {
      for (const e of envs) map[e.id] = appId;
    }
    return map;
  });

  const canGrant = $derived(
    !granting &&
      grantEmail.trim().includes('@') &&
      grantRoleId !== '' &&
      !isEmptySelection(grantSelection),
  );

  async function load(orgId: string) {
    loading = true;
    error = null;
    try {
      const [mem, rls] = await Promise.all([listMembers(orgId), listRoles(orgId)]);
      members = mem;
      roles = rls;
      if (rls.length && !grantRoleId) grantRoleId = rls[0].id;
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  // Resolve app names across every project so app-scoped grants read nicely and
  // the scope tree can list apps under their project.
  //
  // This tracks `sessionStore.projects` instead of sampling it once inside
  // load(): switching orgs empties the store's project list and refills it from
  // its own request, which raced load()'s awaits. Losing that race left
  // `appsByProject` empty for good — no twisties in the scope tree, no app
  // names in the table — with nothing to recompute it short of a page reload.
  $effect(() => {
    const projects = sessionStore.projects;
    let stale = false;
    // Switching orgs restarts the load, so the previous org's apps must stop
    // counting as loaded — otherwise the edit dialog would seed its tree from
    // the old org's app list. The environment cache is keyed by app id, and
    // app ids don't collide across orgs, so this reset isn't required for
    // correctness — it just avoids holding onto a stale org's data.
    appsLoaded = false;
    envsByApp = {};
    loadingEnvApps = new Set();
    void (async () => {
      const appLists = await Promise.all(
        projects.map((p) => listApps(p.id).catch(() => [] as App[])),
      );
      if (stale) return;
      const byProject: Record<string, App[]> = {};
      projects.forEach((p, i) => (byProject[p.id] = appLists[i]));
      appsByProject = byProject;
      appsLoaded = true;
    })();
    return () => {
      stale = true;
    };
  });

  $effect(() => {
    const org = sessionStore.currentOrgId;
    if (org && canReadMembers) void load(org);
    else if (org) loading = false;
  });

  // The grant form holds bare project/app ids belonging to the org that was
  // current when they were ticked, and selectionToScopes() substitutes whatever
  // org is current at submit time. Switching orgs must therefore clear it —
  // otherwise the tree redraws with the new org's projects and nothing ticked
  // while the summary and the enabled Grant button still act on the old org's
  // picks (and an org-level tick would silently retarget the new org).
  $effect(() => {
    // Read into a variable the guard uses: a bare property access would be a
    // side-effect-free expression statement that a minifier is free to drop,
    // taking the dependency — and the reset — with it.
    const org = sessionStore.currentOrgId;
    if (!org) return;
    untrack(() => {
      grantEmail = '';
      // Cleared so load() re-seeds it from the new org's roles; the old id is
      // not in this org's list and the API would reject it.
      grantRoleId = '';
      grantSelection = { ...EMPTY_SELECTION, projects: [], apps: [], envs: [] };
    });
  });

  async function submitGrant(event: SubmitEvent) {
    event.preventDefault();
    const org = sessionStore.currentOrgId;
    if (!org || !canGrant) return;
    granting = true;
    try {
      await createGrant(org, {
        email: grantEmail.trim(),
        role_id: grantRoleId,
        scopes: selectionToScopes(grantSelection, org, projectOfApp, appOfEnv),
      });
      grantEmail = '';
      grantSelection = { ...EMPTY_SELECTION, projects: [], apps: [], envs: [] };
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
    <div class="stack">
      {#if canManage}
      <Card>
        {#snippet header()}
          <h3 class="card-title-inline">Grant access</h3>
          <p class="muted grant-sub">For someone who already has an account, here or in another org.</p>
        {/snippet}
        {#snippet actions()}
          <Button variant="primary" onclick={() => (createOpen = true)}>Create member</Button>
        {/snippet}
        <form class="grant-form" onsubmit={submitGrant}>
          <div class="gf-row">
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
          </div>
          <div class="gf-field">
            <span class="lbl">Scope</span>
            <ScopeTree
              orgId={sessionStore.currentOrg?.id ?? ''}
              orgName={sessionStore.currentOrg?.name ?? 'this org'}
              projects={sessionStore.projects}
              {appsByProject}
              {envsByApp}
              {loadingEnvApps}
              onopenapp={ensureEnvsLoaded}
              value={grantSelection}
              disabled={granting}
              onchange={(next) => (grantSelection = next)}
            />
          </div>
          <div class="gf-actions">
            <Button type="submit" variant="primary" loading={granting} disabled={!canGrant}>Grant</Button>
          </div>
        </form>
      </Card>
    {/if}

    <MembersTable
      {grouped}
      {appsById}
      {envsByApp}
      {projectsById}
      {canManage}
      {removingId}
      {togglingUserId}
      onedit={(userId) => (editingMemberId = userId)}
      ontoggle={requestToggle}
      onremovegrant={removeGrant}
    />

    <Card>
      {#snippet header()}
        <h3 class="card-title-inline">Roles</h3>
      {/snippet}
      {#snippet actions()}
        {#if canManageRoles}
          <Button variant="secondary" size="sm" onclick={openNewRole}>New role</Button>
        {/if}
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
    </div>
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
      orgName={sessionStore.currentOrg.name}
      {roles}
      projects={sessionStore.projects}
      {appsByProject}
      {envsByApp}
      {loadingEnvApps}
      onopenapp={ensureEnvsLoaded}
      onclose={() => (createOpen = false)}
      oncreated={() => load(sessionStore.currentOrg!.id)}
    />
    <EditMemberDialog
      open={editingMemberId !== null}
      orgId={sessionStore.currentOrg.id}
      orgName={sessionStore.currentOrg.name}
      member={editingMember}
      {roles}
      projects={sessionStore.projects}
      {appsByProject}
      {envsByApp}
      {loadingEnvApps}
      onopenapp={ensureEnvsLoaded}
      orgGrants={members}
      ready={appsLoaded}
      onclose={() => (editingMemberId = null)}
      onchanged={() => load(sessionStore.currentOrg!.id)}
      onsaved={() => toastStore.success('Access updated.')}
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
  /* Owns the vertical rhythm for the page's cards. Previously each card carried
     its own margin-bottom via :global(), which silently skipped the members
     table once it moved into its own component and left it stuck to Roles. */
  .stack {
    display: grid;
    gap: 16px;
  }
  .grant-sub {
    font-size: 12.5px;
    margin-top: 3px;
    max-width: 46ch;
  }
  .grant-form {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .gf-row {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    flex-wrap: wrap;
  }
  .gf-actions {
    display: flex;
    justify-content: flex-end;
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
