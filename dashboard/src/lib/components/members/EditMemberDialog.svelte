<script lang="ts">
  import { untrack } from 'svelte';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import ScopeTree from './ScopeTree.svelte';
  import { createGrant, deleteGrant } from '../../api/orgs';
  import { isNormalizedError } from '../../api/client';
  import { authStore } from '../../stores/auth.svelte';
  import {
    emptyBlockKeys,
    grantsToBlocks,
    humanizeGrantError,
    planGrantChanges,
    planIsEmpty,
    planRemovesAllAccess,
    planStripsLastOrgManage,
    type GrantPlan,
    type RoleBlock,
  } from '../../models/grant-plan';
  import { EMPTY_SELECTION } from '../../models/scope-tree';
  import type {
    App,
    AppEnvironment,
    Member,
    MemberGrant,
    Project,
    Role,
    ScopeType,
  } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    orgName: string;
    member: Member | null;
    roles: Role[];
    projects: Project[];
    appsByProject: Record<string, App[]>;
    /** Environments per app, keyed by app id — see ScopeTree's own doc comment.
        Owned by the parent (Members.svelte) so the same cache can be reused by
        the grant form, the create dialog, and the members table. */
    envsByApp: Record<string, AppEnvironment[]>;
    loadingEnvApps: Set<string>;
    onopenapp: (appId: string) => void;
    /** Every grant in the org — the sole-owner pre-flight needs the whole set,
        not just this member's. */
    orgGrants: MemberGrant[];
    /** False until the parent's async app load has settled. Seeding before then
        would file every app-scoped grant as "not visible to you". */
    ready: boolean;
    onclose: () => void;
    onchanged: () => Promise<void> | void;
    onsaved: () => void;
  }

  let {
    open,
    orgId,
    orgName,
    member,
    roles,
    projects,
    appsByProject,
    envsByApp,
    loadingEnvApps,
    onopenapp,
    orgGrants,
    ready,
    onclose,
    onchanged,
    onsaved,
  }: Props = $props();

  type StepStatus = 'ok' | 'error' | 'skipped';
  interface StepResult {
    label: string;
    status: StepStatus;
    message?: string;
  }

  let blocks = $state<RoleBlock[]>([]);
  let saving = $state(false);
  let progress = $state('');
  /** Non-null once a save finished with anything other than every step OK. */
  let results = $state<StepResult[] | null>(null);
  let nextKey = $state(0);

  /** Which (member, generation) the blocks were last seeded from. */
  let seededFor = $state('');
  /** Bumped to force a re-seed from freshly reloaded server state. */
  let reseedToken = $state(0);

  const rolesById = $derived.by(() => {
    const map: Record<string, Role> = {};
    for (const r of roles) map[r.id] = r;
    return map;
  });

  const projectOfApp = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const p of projects) for (const a of appsByProject[p.id] ?? []) map[a.id] = p.id;
    return map;
  });

  const projectNames = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const p of projects) map[p.id] = p.name;
    return map;
  });

  const appNames = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const list of Object.values(appsByProject)) for (const a of list) map[a.id] = a.name;
    return map;
  });

  const appOfEnv = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const [appId, envs] of Object.entries(envsByApp)) {
      for (const e of envs) map[e.id] = appId;
    }
    return map;
  });

  const envNames = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const list of Object.values(envsByApp)) for (const e of list) map[e.id] = e.name;
    return map;
  });

  const knownProjects = $derived(new Set(projects.map((p) => p.id)));
  const knownApps = $derived(new Set(Object.keys(appNames)));
  const knownEnvs = $derived(new Set(Object.keys(appOfEnv)));

  const currentGrants = $derived(member?.grants ?? []);

  const plan = $derived<GrantPlan>(
    member
      ? planGrantChanges(blocks, currentGrants, orgId, projectOfApp, appOfEnv)
      : { additions: [], revocations: [] },
  );

  const emptyKeys = $derived(new Set(emptyBlockKeys(blocks)));
  const destructive = $derived(planRemovesAllAccess(currentGrants, plan));
  const stripsLastOwner = $derived(planStripsLastOrgManage(orgGrants, plan, rolesById));
  const editingSelf = $derived(member !== null && member.user_id === authStore.user?.id);

  const canSave = $derived(
    !saving && !planIsEmpty(plan) && emptyKeys.size === 0 && !stripsLastOwner,
  );

  // Seed one block per role the member holds.
  //
  // Only `open`, `ready`, the member's id and the reseed token are tracked.
  // Everything the seed actually reads — grants, roles, projects, apps — is
  // read inside `untrack`, because a parent reload hands this dialog brand-new
  // prop references (the apps $effect, an org switch, another admin's change)
  // and tracking any of them would wipe the admin's half-finished ticking
  // mid-edit. This is the same hazard CreateMemberDialog's untrack guards.
  $effect(() => {
    if (!open || !member || !ready) return;
    const id = `${member.user_id}#${reseedToken}`;
    untrack(() => {
      if (id === seededFor) return;
      blocks = grantsToBlocks(member!.grants, orgId, knownProjects, knownApps, knownEnvs);
      nextKey = blocks.length;
      results = null;
      progress = '';
      seededFor = id;
    });
  });

  // Closing forgets the seed, so re-opening the same member re-reads the server
  // state rather than restoring abandoned edits.
  $effect(() => {
    if (open) return;
    untrack(() => {
      seededFor = '';
      results = null;
      progress = '';
    });
  });

  // Environments are loaded lazily per app (see envsByApp's doc comment on
  // ScopeTree), so an env-scoped grant seeded before its owning app's
  // environments were fetched cannot be placed by grantsToBlocks — its
  // scope_id isn't in knownEnvs yet — and lands in `unmatched` instead. The
  // moment that app's environments do load (the admin expanded its row, or
  // the shared cache already had them from an earlier dialog this session),
  // promote any now-resolvable unmatched grant into a real tree tick instead
  // of leaving it stuck as an opaque chip forever. This reacts only to data
  // already fetched for a twisty that opened anyway — no extra request.
  //
  // Only `envsByApp` is a tracked read; `blocks` is read and written inside
  // `untrack` so this effect does not re-trigger itself when it moves a grant.
  $effect(() => {
    const apps = envsByApp;
    untrack(() => {
      let anyMoved = false;
      const nextBlocks = blocks.map((b) => {
        if (b.unmatched.length === 0) return b;
        const stillUnmatched: MemberGrant[] = [];
        const newlyKnown: string[] = [];
        for (const g of b.unmatched) {
          const owningApp =
            g.scope_type === 'env'
              ? Object.entries(apps).find(([, envs]) => envs.some((e) => e.id === g.scope_id))
              : undefined;
          if (owningApp) {
            newlyKnown.push(g.scope_id);
            anyMoved = true;
          } else {
            stillUnmatched.push(g);
          }
        }
        if (newlyKnown.length === 0) return b;
        return {
          ...b,
          unmatched: stillUnmatched,
          selection: { ...b.selection, envs: [...b.selection.envs, ...newlyKnown] },
        };
      });
      if (anyMoved) blocks = nextBlocks;
    });
  });

  /** Roles still free to pick — a role may own at most one block. */
  function availableRoles(block: RoleBlock): Role[] {
    const taken = new Set(blocks.filter((b) => b.key !== block.key).map((b) => b.roleId));
    return roles.filter((r) => !taken.has(r.id) || r.id === block.roleId);
  }

  const canAddRole = $derived(blocks.length < roles.length);

  function addRole() {
    const taken = new Set(blocks.map((b) => b.roleId));
    const free = roles.find((r) => !taken.has(r.id));
    if (!free) return;
    blocks = [
      ...blocks,
      {
        key: `new-${nextKey}`,
        roleId: free.id,
        selection: { ...EMPTY_SELECTION, projects: [], apps: [], envs: [] },
        unmatched: [],
      },
    ];
    nextKey += 1;
  }

  function removeBlock(key: string) {
    blocks = blocks.filter((b) => b.key !== key);
  }

  function setSelection(key: string, next: RoleBlock['selection']) {
    blocks = blocks.map((b) => (b.key === key ? { ...b, selection: next } : b));
  }

  function setRole(key: string, roleId: string) {
    blocks = blocks.map((b) => (b.key === key ? { ...b, roleId } : b));
  }

  function dropUnmatched(key: string, grantId: string) {
    blocks = blocks.map((b) =>
      b.key === key ? { ...b, unmatched: b.unmatched.filter((g) => g.id !== grantId) } : b,
    );
  }

  function roleName(id: string): string {
    return rolesById[id]?.name ?? 'Unknown role';
  }

  /** An env's own name, prefixed with its app's name when known — mirrors the
      "App: Project / Name" composition one level down. `scope_id` carries no
      FK, so a deleted/invisible env or app falls back to a truncated id. */
  function envLabel(envId: string): string {
    const name = envNames[envId] ?? envId.slice(0, 8);
    const appId = appOfEnv[envId];
    const appName = appId !== undefined ? appNames[appId] : undefined;
    return appName ? `${appName} / ${name}` : name;
  }

  /** Shared by scopeLabel (MemberGrant) and every place below that names a
      ScopeRef out of a GrantPlan — kept in one place so a fifth scope level
      only needs one new branch, not three kept in sync by hand. */
  function scopeRefLabel(s: { scope_type: ScopeType; scope_id: string }): string {
    if (s.scope_type === 'org') return orgName;
    if (s.scope_type === 'project') return projectNames[s.scope_id] ?? s.scope_id.slice(0, 8);
    if (s.scope_type === 'app') return appNames[s.scope_id] ?? s.scope_id.slice(0, 8);
    if (s.scope_type === 'env') return envLabel(s.scope_id);
    return s.scope_id.slice(0, 8);
  }

  function scopeLabel(g: MemberGrant): string {
    return scopeRefLabel(g);
  }

  const addSummary = $derived(
    plan.additions
      .flatMap((a) => a.scopes.map((s) => `${roleName(a.roleId)} on ${scopeRefLabel(s)}`))
      .join(', '),
  );

  const removeSummary = $derived(
    plan.revocations.map((g) => `${roleName(g.role_id)} on ${scopeLabel(g)}`).join(', '),
  );

  /** Two roles reaching the same scope is legal — permissions union. Say so. */
  const overlapNote = $derived.by(() => {
    const orgBlocks = blocks.filter((b) => b.selection.org);
    if (orgBlocks.length < 2) return '';
    const names = orgBlocks.map((b) => roleName(b.roleId)).join(' and ');
    return `${names} both cover ${orgName} — permissions add up.`;
  });

  function humanize(message: string): string {
    return humanizeGrantError(message, {
      projects: projectNames,
      apps: appNames,
      orgName,
    });
  }

  async function submit() {
    if (!member || !canSave) return;
    saving = true;
    results = null;
    const steps: StepResult[] = [];
    const total = plan.additions.length + plan.revocations.length;
    let done = 0;
    let networkFailures = 0;
    let anyGrantFailed = false;

    // Phase 1 — grant before revoking, always. Every intermediate state is then
    // a superset of both the old and the intended access, so an interruption or
    // a rejection can only ever leave the member over-privileged (visible, and
    // one more Save from correct) rather than locked out.
    for (const add of plan.additions) {
      done += 1;
      progress = `Applying ${done} of ${total}…`;
      const label = `Grant ${roleName(add.roleId)} on ${add.scopes.map(scopeRefLabel).join(', ')}`;
      try {
        await createGrant(orgId, {
          email: member.email,
          role_id: add.roleId,
          scopes: add.scopes,
        });
        steps.push({ label, status: 'ok' });
        networkFailures = 0;
      } catch (err) {
        anyGrantFailed = true;
        steps.push({ label, status: 'error', message: humanize(readMessage(err)) });
        if (isNormalizedError(err) && err.isNetwork) networkFailures += 1;
        else networkFailures = 0;
        if (networkFailures >= 2) break;
      }
    }

    // The coupling rule, and the load-bearing half of the safety story: if any
    // addition was refused, skip every revocation. Demoting a member's only
    // grant would otherwise strip their access outright when the replacement
    // 403s — running the delete anyway is the one way this dialog could
    // destroy access.
    for (const g of plan.revocations) {
      const label = `Remove ${roleName(g.role_id)} on ${scopeLabel(g)}`;
      if (anyGrantFailed) {
        steps.push({
          label,
          status: 'skipped',
          message: 'Kept, because some new access could not be granted.',
        });
        continue;
      }
      if (networkFailures >= 2) {
        steps.push({ label, status: 'skipped', message: 'Stopped — the API is unreachable.' });
        continue;
      }
      done += 1;
      progress = `Applying ${done} of ${total}…`;
      try {
        await deleteGrant(g.id);
        steps.push({ label, status: 'ok' });
        networkFailures = 0;
      } catch (err) {
        // Already gone is the outcome we wanted. Reporting it as a failure
        // would strand the dialog in its result panel after any retry, or when
        // another admin removed the same grant first.
        if (isNormalizedError(err) && err.status === 404) {
          steps.push({ label, status: 'ok', message: 'Already removed.' });
          continue;
        }
        let message = humanize(readMessage(err));
        // DELETE's 403 is the bare "you do not have access", unlike POST's,
        // which names the scope — so say which check actually failed.
        if (isNormalizedError(err) && err.status === 403) {
          message = `You don't hold every permission in "${roleName(g.role_id)}" at this scope, so you can't remove this grant.`;
        }
        steps.push({ label, status: 'error', message });
        if (isNormalizedError(err) && err.isNetwork) networkFailures += 1;
        else networkFailures = 0;
      }
    }

    progress = '';
    // Reload unconditionally: the server may have moved even on total failure.
    await onchanged();
    saving = false;

    if (steps.every((s) => s.status === 'ok')) {
      onsaved();
      onclose();
    } else {
      results = steps;
    }
  }

  function readMessage(err: unknown): string {
    if (isNormalizedError(err)) return err.message;
    if (err instanceof Error) return err.message;
    return 'Something went wrong';
  }

  /** Re-seed from the now-authoritative server state and return to the form. */
  function backToEditing() {
    results = null;
    reseedToken += 1;
  }
</script>

<Modal
  {open}
  size="lg"
  title={member ? `Edit access — ${member.name || member.email}` : 'Edit access'}
  dismissible={!saving}
  onclose={onclose}
>
  {#if !member}
    <p class="lede">No member selected.</p>
  {:else if !ready}
    <div class="center"><Spinner size={24} /></div>
  {:else if results}
    <p class="err-msg head-msg">Some changes were not applied.</p>
    <ul class="results">
      {#each results as step, i (i)}
        <li class="result-row">
          <Badge tone={step.status === 'ok' ? 'success' : step.status === 'error' ? 'error' : 'neutral'} size="sm">
            {step.status === 'ok' ? 'done' : step.status === 'error' ? 'failed' : 'kept'}
          </Badge>
          <div class="r-body">
            <span class="r-label">{step.label}</span>
            {#if step.message}<span class="r-msg">{step.message}</span>{/if}
          </div>
        </li>
      {/each}
    </ul>
  {:else}
    <div class="identity">
      <div class="id-main">
        <span class="id-name">{member.name || member.email}</span>
        {#if !member.is_active}<Badge tone="warning" size="sm">Deactivated</Badge>{/if}
      </div>
      {#if member.name}<span class="id-email">{member.email}</span>{/if}
    </div>
    <p class="lede">Tick what they can reach. Each role carries its own scope selection.</p>

    {#if !member.is_active}
      <p class="warning">
        This account is deactivated. You can remove access, but new grants will be refused until
        it is reactivated.
      </p>
    {/if}
    {#if editingSelf}
      <p class="warning">
        You are editing your own access. Removing these grants can end your ability to manage
        members.
      </p>
    {/if}

    <div class="blocks">
      {#each blocks as block (block.key)}
        {@const isEmpty = emptyKeys.has(block.key)}
        <div class="block" class:invalid={isEmpty}>
          <div class="b-head">
            <div class="gf-field">
              <span class="lbl">Role</span>
              <select
                class="sel"
                value={block.roleId}
                aria-label="Role"
                disabled={saving}
                onchange={(e) => setRole(block.key, e.currentTarget.value)}
              >
                {#each availableRoles(block) as role (role.id)}
                  <option value={role.id}>{role.name}</option>
                {/each}
              </select>
            </div>
            <Button
              variant="ghost"
              size="sm"
              disabled={saving}
              onclick={() => removeBlock(block.key)}
            >
              Remove this role
            </Button>
          </div>

          <div class="gf-field">
            <span class="lbl">Scope</span>
            <ScopeTree
              {orgId}
              {orgName}
              {projects}
              {appsByProject}
              {envsByApp}
              {loadingEnvApps}
              {onopenapp}
              value={block.selection}
              disabled={saving}
              onchange={(next) => setSelection(block.key, next)}
            />
          </div>

          {#if block.unmatched.length}
            <div class="unmatched">
              <span class="lbl">Scopes not visible to you</span>
              <p class="u-note">Kept as they are unless you remove them.</p>
              {#each block.unmatched as g (g.id)}
                <div class="u-row">
                  <code>{g.scope_type} {g.scope_id.slice(0, 8)}…</code>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={saving}
                    onclick={() => dropUnmatched(block.key, g.id)}
                  >
                    Remove
                  </Button>
                </div>
              {/each}
            </div>
          {/if}

          {#if isEmpty}
            <p class="err-msg">Pick at least one scope, or remove this role.</p>
          {/if}
        </div>
      {/each}
    </div>

    {#if blocks.length === 0}
      <p class="empty">
        This member holds no access in {orgName}. Add a role to give them some.
      </p>
    {/if}

    <div class="add-row">
      <Button variant="secondary" size="sm" disabled={saving || !canAddRole} onclick={addRole}>
        Add a role
      </Button>
    </div>

    {#if overlapNote}<p class="note">{overlapNote}</p>{/if}

    {#if !planIsEmpty(plan)}
      <div class="preview">
        {#if addSummary}<p class="p-add">Adding: {addSummary}.</p>{/if}
        {#if removeSummary}
          <p class="warning">Removing: {removeSummary}.</p>
        {/if}
      </div>
    {/if}

    {#if stripsLastOwner}
      <p class="warning">
        Cannot remove the last grant with org:manage — assign it to another member first.
      </p>
    {:else if destructive}
      <p class="warning">
        This removes every grant for {member.email} in {orgName}. They will disappear from this
        list and lose access to the org. Their account is not deleted — it stays active and they
        can still sign in.
      </p>
    {/if}
  {/if}

  {#snippet footer()}
    {#if results}
      <Button variant="ghost" onclick={onclose}>Close</Button>
      <Button variant="primary" onclick={backToEditing}>Back to editing</Button>
    {:else}
      {#if progress}<span class="progress">{progress}</span>{/if}
      <Button variant="ghost" onclick={onclose} disabled={saving}>Cancel</Button>
      <Button
        variant={destructive ? 'danger' : 'primary'}
        disabled={!canSave}
        loading={saving}
        onclick={submit}
      >
        {destructive ? 'Remove all access' : 'Save changes'}
      </Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .center {
    display: grid;
    place-items: center;
    padding: 40px;
  }
  .identity {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-bottom: 10px;
  }
  .id-main {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .id-name {
    font-size: 13.5px;
    font-weight: 600;
    color: var(--text);
  }
  .id-email {
    font-size: 12.5px;
    color: var(--text-faint);
  }
  .lede {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 14px;
  }
  .blocks {
    display: flex;
    flex-direction: column;
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 0;
    border-top: 1px solid var(--border);
  }
  .block:first-child {
    border-top: none;
    padding-top: 0;
  }
  .block.invalid .lbl {
    color: var(--error);
  }
  .b-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 10px;
  }
  .gf-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
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
    width: 100%;
  }
  .sel option {
    background: var(--surface);
    color: var(--text);
  }
  .unmatched {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .u-note {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .u-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .u-row code {
    font-size: 12px;
    color: var(--text-muted);
  }
  .empty {
    font-size: 12.5px;
    color: var(--text-faint);
    padding: 6px 0;
  }
  .add-row {
    display: flex;
    margin-top: 14px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .note {
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 10px;
  }
  .preview {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 12px;
  }
  .p-add {
    font-size: 12.5px;
    color: var(--text-muted);
  }
  .warning {
    font-size: 12.5px;
    color: var(--warning);
    background: var(--warning-soft);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent);
    border-radius: var(--radius);
    padding: 8px 12px;
    margin-top: 10px;
  }
  .preview .warning {
    margin-top: 0;
  }
  .progress {
    font-size: 12.5px;
    color: var(--text-muted);
    margin-right: auto;
    align-self: center;
  }
  .results {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .result-row {
    display: flex;
    align-items: flex-start;
    gap: 9px;
  }
  .r-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .r-label {
    font-size: 13px;
    color: var(--text);
  }
  .r-msg {
    font-size: 12px;
    color: var(--text-muted);
  }
  .head-msg {
    margin-bottom: 12px;
  }
  .err-msg {
    color: var(--error);
    font-size: 12.5px;
  }
</style>
