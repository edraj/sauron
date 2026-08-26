<script lang="ts">
  import { t } from '../lib/i18n';
  import { untrack } from 'svelte';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import CreateMemberDialog from '../lib/components/members/CreateMemberDialog.svelte';
  import EditMemberDialog from '../lib/components/members/EditMemberDialog.svelte';
  import ResetPasswordDialog from '../lib/components/members/ResetPasswordDialog.svelte';
  import MembersTable from '../lib/components/members/MembersTable.svelte';
  import ScopeTree from '../lib/components/members/ScopeTree.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { lockedBy } from '../lib/models/page-access';
  import { authStore } from '../lib/stores/auth.svelte';
  import {
    listMembers,
    listRoles,
    createGrant,
    deleteGrant,
    setMemberActive,
    resetMemberPassword,
  } from '../lib/api/orgs';
  import { listApps } from '../lib/api/apps';
  import { listEnvironments } from '../lib/api/environments';
  import { revokeMemberSessions } from '../lib/api/account';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    groupMembers,
    type App,
    type AppEnvironment,
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

  // Cached view (lib/stores/cached-view.svelte.ts): the roster paints instantly
  // on a revisit, then refreshes behind the rows.
  //
  // ONE view holding both responses rather than two: the grant form's role
  // picker is built from `roles` while the table renders `members`, and a cache
  // that could serve one fresh beside the other stale would offer a role that
  // no longer exists to a roster that already reflects its deletion.
  const view = new CachedView<{ members: MemberGrant[]; roles: Role[] }>();
  const members = $derived(view.data?.members ?? []);
  const roles = $derived(view.data?.roles ?? []);
  // Keyed by project because the scope tree renders by project; `appsById` is
  // the flattened view the table and the scope labels want.
  let appsByProject = $state<Record<string, App[]>>({});
  // The app load below is async and races the dialogs opening. Until it
  // settles, EditMemberDialog cannot tell an app-scoped grant from one whose
  // target it can't see, so it waits rather than seeding a wrong tree.
  let appsLoaded = $state(false);
  const loading = $derived(view.loading);
  // Mutations report through the same banner as the load, and a `$derived`
  // cannot be assigned to — so they keep their own state and this is the fold.
  let actionError = $state<string | null>(null);
  const error = $derived(actionError ?? view.error);

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
  let envsByApp = $state<Record<string, AppEnvironment[]>>({});
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

  // Create / edit member dialogs.
  let createOpen = $state(false);
  let editingMemberId = $state<string | null>(null);
  let togglingUserId = $state<string | null>(null);
  let deactivateTarget = $state<Member | null>(null);
  let revokingUserId = $state<string | null>(null);
  let pendingRevoke = $state<Member | null>(null);

  // One piece of state for both directions: the table never offers a member
  // both a reset and a cancel, so a second flag could only ever disagree with
  // the row that opened the dialog.
  let resetTarget = $state<{ member: Member; action: 'reset' | 'cancel' } | null>(null);
  let resetBusy = $state(false);

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

  // Every members endpoint resolves through `authorize_org`
  // (orgs.rs:160,552,809,1020), which no project- or app-scoped grant can
  // satisfy — hence `level: 'org'` on all of these. Without it, a member whose
  // grant carries `member:manage` at project scope saw every control here lit
  // and got a 403 from each one.
  const manageLock = $derived(lockedBy('member:manage', { level: 'org' }));
  // Deliberately not `manageLock`: a custom role may hold `member:manage`
  // without `member:credential`, and showing this button to that role means
  // every click 403s.
  const revokeLock = $derived(lockedBy('member:credential', { level: 'org' }));
  // Password reset re-checks BOTH server-side (orgs.rs:1020 + 755), so report
  // whichever is missing — `member:credential` first, since it is the narrower
  // one deliberately carved out of `member:manage`.
  const credentialLock = $derived(revokeLock ?? manageLock);
  const canReadMembers = $derived(sessionStore.can('member:read', { level: 'org' }));

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

  /**
   * `force` bypasses the fresh-window short-circuit. Every mutation on this page
   * re-lists through it: a re-list that joined a flight issued before the write
   * returns the pre-write roster, and `set` would then cache it — so the grant
   * just revoked reappears and stays for the whole fresh window.
   */
  async function load(orgId: string, force = false) {
    actionError = null;
    await view.load(
      viewKey('members.roster', orgId),
      async () => {
        const [members, roles] = await Promise.all([listMembers(orgId), listRoles(orgId)]);
        return { members, roles };
      },
      force,
    );
  }

  async function reload() {
    const org = sessionStore.currentOrgId;
    if (!org) return;
    viewCache.invalidate('members.roster');
    await load(org, true);
  }

  // Default the grant form's role picker once the catalogue is known. An effect
  // rather than a line inside load(), because `roles` is now derived from the
  // view and a cache HIT populates it without load() ever reaching its body.
  $effect(() => {
    const first = roles[0];
    if (first && !grantRoleId) grantRoleId = first.id;
  });

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
    // No read permission means no request is issued at all, and `loading` starts
    // true — so without this the page spins forever on a fetch that never
    // happened. `idle()` is exactly that state: nothing to load, not an error.
    else if (org) view.idle();
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
      await load(org, true);
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
      // Re-list rather than filtering locally: `members` is derived from the
      // cache's own payload, so a local edit would either be discarded by the
      // next read or — if written through the proxy — corrupt the cached roster
      // for every later visit.
      await reload();
      toastStore.success('Access removed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      removingId = null;
    }
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
      await load(org.id, true);
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

  async function confirmPasswordReset() {
    const org = sessionStore.currentOrg;
    const target = resetTarget;
    if (!org || !target) return;
    resetBusy = true;
    try {
      await resetMemberPassword(org.id, target.member.user_id, target.action);
      toastStore.success(
        target.action === 'reset'
          ? `${target.member.email} has been emailed a link to set a new password.`
          : `${target.member.email} can sign in with their existing password again.`,
      );
      resetTarget = null;
      await load(org.id, true);
    } catch (err) {
      // The backend's 409s carry the actionable text (self, inactive,
      // cross-org) and its 503 names the missing setting — surface both
      // verbatim, exactly as toggleActive already does.
      toastStore.error(errorMessage(err));
    } finally {
      resetBusy = false;
    }
  }

  function requestRevokeSessions(member: Member) {
    pendingRevoke = member;
  }

  async function confirmRevokeSessions() {
    const member = pendingRevoke;
    pendingRevoke = null;
    const org = sessionStore.currentOrg;
    if (!member || !org) return;
    revokingUserId = member.user_id;
    try {
      const n = await revokeMemberSessions(org.id, member.user_id);
      toastStore.success(
        `${member.email} was signed out of ${n === 1 ? '1 device' : `${n} devices`}.`,
      );
    } catch (err) {
      // The backend's 403/404/409s carry the actionable text (outranks you,
      // not a member here, belongs to another organization, self-target) —
      // surface it verbatim.
      toastStore.error(errorMessage(err));
    } finally {
      revokingUserId = null;
    }
  }
</script>

<AdminShell>
  <div class="head">
    <div>
      <h1 class="page-title">{t('members.title')}</h1>
      <p class="muted sub">People with access to {sessionStore.currentOrg?.name ?? 'this org'}.</p>
    </div>
  </div>

  <!-- The `member:read` gate is the hoisted AppShell's now (App.svelte): it
       resolves /admin/members through PAGE_ACCESS and renders PermissionDenied
       instead of this page, so the bespoke "No access" card that used to live
       here would be unreachable. -->
  {#if loading}
    <Skeleton rows={6} />
  {:else if error}
    <Card><p class="err-msg">{error}</p></Card>
  {:else}
    <div class="stack">
      <Card>
        {#snippet header()}
          <h3 class="card-title-inline">{t('members.grantAccess')}</h3>
          <p class="muted grant-sub">{t('members.existingAccount')}</p>
        {/snippet}
        {#snippet actions()}
          <Button variant="primary" lockedReason={manageLock} onclick={() => (createOpen = true)}>
            {t('members.create')}
          </Button>
        {/snippet}
        <form class="grant-form" onsubmit={submitGrant}>
          <div class="gf-row">
            <div class="gf-field">
              <Input label={t('common.email')} type="email" bind:value={grantEmail} placeholder={t('members.placeholder.email')} required />
            </div>
            <div class="gf-field">
              <span class="lbl">{t('members.column.role')}</span>
              <select class="sel" bind:value={grantRoleId} aria-label={t('members.column.role')}>
                {#each roles as role (role.id)}
                  <option value={role.id}>{role.name}</option>
                {/each}
              </select>
            </div>
          </div>
          <div class="gf-field">
            <span class="lbl">{t('members.column.scope')}</span>
            <ScopeTree
              orgId={sessionStore.currentOrg?.id ?? ''}
              orgName={sessionStore.currentOrg?.name ?? 'this org'}
              projects={sessionStore.projects}
              {appsByProject}
              {envsByApp}
              {loadingEnvApps}
              onopenapp={ensureEnvsLoaded}
              value={grantSelection}
              disabled={granting || manageLock !== null}
              onchange={(next) => (grantSelection = next)}
            />
          </div>
          <div class="gf-actions">
            <Button
              type="submit"
              variant="primary"
              loading={granting}
              disabled={!canGrant}
              lockedReason={manageLock}
            >
              {t('members.grant')}
            </Button>
          </div>
        </form>
      </Card>

    <MembersTable
      {grouped}
      {appsById}
      {envsByApp}
      {projectsById}
      {manageLock}
      {removingId}
      {togglingUserId}
      {revokeLock}
      {revokingUserId}
      onrevokesessions={requestRevokeSessions}
      onedit={(userId) => (editingMemberId = userId)}
      ontoggle={requestToggle}
      currentUserId={authStore.user?.id ?? ''}
      {credentialLock}
      onresetpassword={(m, a) => (resetTarget = { member: m, action: a })}
      onremovegrant={removeGrant}
    />
    </div>
  {/if}

  {#if sessionStore.currentOrg}
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
      title={t('members.confirmDeactivate')}
      message={`${deactivateTarget.email} will be signed out of every device and won't be able to sign in until reactivated. Their access grants are kept.`}
      confirmLabel={t('members.deactivate')}
      danger
      onconfirm={confirmDeactivate}
      oncancel={() => (deactivateTarget = null)}
    />
  {/if}

  {#if resetTarget}
    <ResetPasswordDialog
      member={resetTarget.member}
      action={resetTarget.action}
      busy={resetBusy}
      onconfirm={confirmPasswordReset}
      oncancel={() => (resetTarget = null)}
    />
  {/if}

  {#if pendingRevoke}
    <ConfirmDialog
      open
      title={t('members.signOutAll')}
      message={`${pendingRevoke.name || pendingRevoke.email} will be signed out on every device and will have to log in again. Their account stays active.`}
      confirmLabel={t('auth.signOut')}
      danger
      onconfirm={() => void confirmRevokeSessions()}
      oncancel={() => (pendingRevoke = null)}
    />
  {/if}
</AdminShell>

<style>
  .head {
    margin-bottom: 18px;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  /* Owns the vertical rhythm for the page's cards. Previously each card carried
     its own margin-bottom via :global(), which silently skipped the members
     table once it moved into its own component and left it stuck to the card
     below it. */
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
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }
</style>
