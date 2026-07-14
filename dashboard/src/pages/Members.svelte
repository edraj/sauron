<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import {
    listMembers,
    listRoles,
    createGrant,
    deleteGrant,
    createRole,
  } from '../lib/api/orgs';
  import { listApps } from '../lib/api/apps';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { initials } from '../lib/utils/format';
  import type { App, MemberGrant, Permission, Role, ScopeType } from '../lib/models';

  const ALL_PERMISSIONS: Permission[] = [
    'issue:read',
    'issue:write',
    'event:read',
    'app:read',
    'app:create',
    'app:update',
    'app:delete',
    'app:rotate_key',
    'project:read',
    'project:create',
    'project:update',
    'project:delete',
    'member:read',
    'member:manage',
    'role:manage',
    'org:manage',
  ];

  let members = $state<MemberGrant[]>([]);
  let roles = $state<Role[]>([]);
  let appsById = $state<Record<string, App>>({});
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Invite / grant form
  let inviteEmail = $state('');
  let inviteRoleId = $state('');
  let inviteScopeKey = $state(''); // `${scope_type}:${scope_id}`
  let inviting = $state(false);
  let removingId = $state<string | null>(null);

  // Create role form
  let showRoleForm = $state(false);
  let roleName = $state('');
  let roleDescription = $state('');
  let rolePerms = $state<Record<string, boolean>>({});
  let creatingRole = $state(false);

  const canManage = $derived(sessionStore.can('member:manage'));
  const canReadMembers = $derived(sessionStore.can('member:read'));
  const canManageRoles = $derived(sessionStore.can('role:manage'));

  interface ScopeOption {
    key: string;
    label: string;
    scope_type: ScopeType;
    scope_id: string;
  }

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

  function scopeLabel(member: MemberGrant): string {
    if (member.scope_type === 'org') return 'Org';
    if (member.scope_type === 'project') {
      const p = sessionStore.projects.find((x) => x.id === member.scope_id);
      return `Project: ${p?.name ?? member.scope_id.slice(0, 8)}`;
    }
    const a = appsById[member.scope_id];
    return `App: ${a?.name ?? member.scope_id.slice(0, 8)}`;
  }

  function scopeTone(type: ScopeType): 'primary' | 'info' | 'neutral' {
    if (type === 'org') return 'primary';
    if (type === 'project') return 'info';
    return 'neutral';
  }

  async function load(orgId: string) {
    loading = true;
    error = null;
    try {
      const [mem, rls] = await Promise.all([listMembers(orgId), listRoles(orgId)]);
      members = mem;
      roles = rls;
      if (rls.length && !inviteRoleId) inviteRoleId = rls[0].id;
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

  async function submitInvite(event: SubmitEvent) {
    event.preventDefault();
    const org = sessionStore.currentOrgId;
    if (!org || inviting || !inviteEmail.trim() || !inviteRoleId || !inviteScopeKey) return;
    const opt = scopeOptions.find((o) => o.key === inviteScopeKey);
    if (!opt) return;
    inviting = true;
    try {
      await createGrant(org, {
        email: inviteEmail.trim(),
        role_id: inviteRoleId,
        scope_type: opt.scope_type,
        scope_id: opt.scope_id,
      });
      inviteEmail = '';
      inviteScopeKey = '';
      await load(org);
      toastStore.success('Access granted.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      inviting = false;
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

  async function submitRole(event: SubmitEvent) {
    event.preventDefault();
    const org = sessionStore.currentOrgId;
    if (!org || creatingRole || !roleName.trim()) return;
    const permissions = ALL_PERMISSIONS.filter((p) => rolePerms[p]);
    creatingRole = true;
    try {
      const role = await createRole(org, {
        name: roleName.trim(),
        description: roleDescription.trim() || undefined,
        permissions,
      });
      roles = [...roles, role];
      roleName = '';
      roleDescription = '';
      rolePerms = {};
      showRoleForm = false;
      toastStore.success('Role created.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      creatingRole = false;
    }
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
      <Card title="Grant access" class="grant-card">
        <form class="grant-form" onsubmit={submitInvite}>
          <div class="gf-field">
            <Input label="Email" type="email" bind:value={inviteEmail} placeholder="teammate@company.com" required />
          </div>
          <div class="gf-field">
            <span class="lbl">Role</span>
            <select class="sel" bind:value={inviteRoleId} aria-label="Role">
              {#each roles as role (role.id)}
                <option value={role.id}>{role.name}</option>
              {/each}
            </select>
          </div>
          <div class="gf-field">
            <span class="lbl">Scope</span>
            <select class="sel" bind:value={inviteScopeKey} aria-label="Scope">
              <option value="" disabled>Select scope…</option>
              {#each scopeOptions as opt (opt.key)}
                <option value={opt.key}>{opt.label}</option>
              {/each}
            </select>
          </div>
          <Button type="submit" variant="primary" loading={inviting}>Grant</Button>
        </form>
      </Card>
    {/if}

    <Card padding="none">
      <div class="table-scroll">
        <table class="members">
          <thead>
            <tr>
              <th>Member</th>
              <th>Role</th>
              <th>Scope</th>
              {#if canManage}<th class="col-act"></th>{/if}
            </tr>
          </thead>
          <tbody>
            {#each members as m (m.id)}
              <tr>
                <td>
                  <div class="member-cell">
                    <span class="m-avatar">{initials(m.name || m.email)}</span>
                    <div class="m-meta">
                      <span class="m-name">{m.name || m.email}</span>
                      {#if m.name}<span class="m-email">{m.email}</span>{/if}
                    </div>
                  </div>
                </td>
                <td><span class="role-tag">{m.role_name}</span></td>
                <td>
                  <Badge tone={scopeTone(m.scope_type)} size="sm">{scopeLabel(m)}</Badge>
                </td>
                {#if canManage}
                  <td class="col-act">
                    <Button
                      variant="ghost"
                      size="sm"
                      loading={removingId === m.id}
                      onclick={() => removeGrant(m.id)}
                    >
                      Remove
                    </Button>
                  </td>
                {/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </Card>

    <Card class="roles-card">
      {#snippet header()}
        <div class="roles-head">
          <h3 class="card-title-inline">Roles</h3>
          {#if canManageRoles}
            <Button variant="secondary" size="sm" onclick={() => (showRoleForm = !showRoleForm)}>
              {showRoleForm ? 'Cancel' : 'New role'}
            </Button>
          {/if}
        </div>
      {/snippet}

      {#if showRoleForm && canManageRoles}
        <form class="role-form" onsubmit={submitRole}>
          <div class="role-fields">
            <Input label="Role name" bind:value={roleName} placeholder="Support" required />
            <Input label="Description" bind:value={roleDescription} placeholder="Read + resolve issues" />
          </div>
          <span class="lbl perms-label">Permissions</span>
          <div class="perms-grid">
            {#each ALL_PERMISSIONS as perm (perm)}
              <label class="perm">
                <input type="checkbox" bind:checked={rolePerms[perm]} />
                <span class="mono">{perm}</span>
              </label>
            {/each}
          </div>
          <Button type="submit" variant="primary" loading={creatingRole}>Create role</Button>
        </form>
      {/if}

      <ul class="role-list">
        {#each roles as role (role.id)}
          <li class="role-row">
            <div class="r-main">
              <span class="r-name">{role.name}</span>
              {#if role.is_system}<Badge tone="neutral" size="sm">system</Badge>{/if}
              {#if role.description}<span class="r-desc muted">{role.description}</span>{/if}
            </div>
            <span class="r-count muted">{role.permissions.length} permissions</span>
          </li>
        {/each}
      </ul>
    </Card>
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
  .table-scroll {
    overflow-x: auto;
  }
  table.members {
    width: 100%;
    border-collapse: collapse;
    font-size: 13.5px;
  }
  thead th {
    text-align: left;
    padding: 12px 16px;
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
  }
  td {
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
  }
  tbody tr:last-child td {
    border-bottom: none;
  }
  .col-act {
    text-align: right;
    width: 1%;
    white-space: nowrap;
  }
  .member-cell {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .m-avatar {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 11px;
    font-weight: 650;
    flex-shrink: 0;
  }
  .m-meta {
    display: flex;
    flex-direction: column;
    line-height: 1.3;
  }
  .m-name {
    font-weight: 560;
  }
  .m-email {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .role-tag {
    font-weight: 560;
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
  .role-form {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding-bottom: 18px;
    margin-bottom: 6px;
    border-bottom: 1px solid var(--border);
  }
  .role-fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .perms-label {
    margin-bottom: -4px;
  }
  .perms-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 6px 14px;
  }
  .perm {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12.5px;
    color: var(--text-muted);
  }
  .perm input {
    accent-color: var(--primary);
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
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }

  @media (max-width: 640px) {
    .role-fields {
      grid-template-columns: 1fr;
    }
  }
</style>
