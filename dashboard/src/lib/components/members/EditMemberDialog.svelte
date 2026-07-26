<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import { createGrant, updateGrant } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Member, MemberGrant, Role, ScopeOption } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    member: Member | null;
    roles: Role[];
    scopeOptions: ScopeOption[];
    onclose: () => void;
    onchanged: () => void;
  }

  let { open, orgId, member, roles, scopeOptions, onclose, onchanged }: Props = $props();

  /** Pending role/scope selection per existing grant id. */
  let edits = $state<Record<string, { roleId: string; scopeKey: string }>>({});
  /** Which grant id is mid-save, if any — each PATCH is independent, so only
      that row's button shows a spinner and the rest stay usable. */
  let savingGrantId = $state<string | null>(null);
  /** Per-grant error, keyed by grant id, so one row's failed guard (last-owner,
      duplicate, escalation) doesn't get confused with another's. */
  let errors = $state<Record<string, string>>({});

  // The "add another grant" row.
  let addRoleId = $state('');
  let addScopeKey = $state('');
  let addSaving = $state(false);
  let addError = $state<string | null>(null);

  // Repopulate whenever the dialog opens on a (possibly different) member.
  $effect(() => {
    if (!open || !member) return;
    const next: Record<string, { roleId: string; scopeKey: string }> = {};
    for (const g of member.grants) {
      next[g.id] = { roleId: g.role_id, scopeKey: `${g.scope_type}:${g.scope_id}` };
    }
    edits = next;
    errors = {};
    savingGrantId = null;
    addRoleId = roles[0]?.id ?? '';
    addScopeKey = scopeOptions[0]?.key ?? '';
    addSaving = false;
    addError = null;
  });

  function isDirty(grant: MemberGrant): boolean {
    const e = edits[grant.id];
    return (
      !!e &&
      (e.roleId !== grant.role_id || e.scopeKey !== `${grant.scope_type}:${grant.scope_id}`)
    );
  }

  async function saveGrant(grant: MemberGrant) {
    const e = edits[grant.id];
    const scope = scopeOptions.find((s) => s.key === e?.scopeKey);
    if (!e || !scope || savingGrantId) return;
    savingGrantId = grant.id;
    errors = { ...errors, [grant.id]: '' };
    try {
      await updateGrant(grant.id, {
        role_id: e.roleId,
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
      });
      onchanged();
    } catch (err) {
      errors = { ...errors, [grant.id]: errorMessage(err) };
    } finally {
      savingGrantId = null;
    }
  }

  const canAdd = $derived(!addSaving && addRoleId !== '' && addScopeKey !== '');

  async function addGrant() {
    if (!member || !canAdd) return;
    const scope = scopeOptions.find((s) => s.key === addScopeKey);
    if (!scope) return;
    addSaving = true;
    addError = null;
    try {
      await createGrant(orgId, {
        email: member.email,
        role_id: addRoleId,
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
      });
      addScopeKey = scopeOptions[0]?.key ?? '';
      onchanged();
    } catch (err) {
      addError = errorMessage(err);
    } finally {
      addSaving = false;
    }
  }
</script>

<Modal {open} title={member ? `Edit access — ${member.name || member.email}` : 'Edit access'} onclose={onclose}>
  {#if member}
    <div class="grants">
      {#each member.grants as grant (grant.id)}
        {@const e =
          edits[grant.id] ??
          (edits[grant.id] = {
            roleId: grant.role_id,
            scopeKey: `${grant.scope_type}:${grant.scope_id}`,
          })}
        <div class="grant-row">
          <div class="gf-field">
            <span class="lbl">Role</span>
            <select class="sel" bind:value={e.roleId} aria-label="Role">
              {#each roles as role (role.id)}
                <option value={role.id}>{role.name}</option>
              {/each}
            </select>
          </div>
          <div class="gf-field">
            <span class="lbl">Scope</span>
            <select class="sel" bind:value={e.scopeKey} aria-label="Scope">
              {#each scopeOptions as opt (opt.key)}
                <option value={opt.key}>{opt.label}</option>
              {/each}
            </select>
          </div>
          <Button
            variant="secondary"
            size="sm"
            disabled={!isDirty(grant)}
            loading={savingGrantId === grant.id}
            onclick={() => saveGrant(grant)}
          >
            Save
          </Button>
        </div>
        {#if errors[grant.id]}<p class="err-msg">{errors[grant.id]}</p>{/if}
      {/each}
    </div>

    <div class="add-section">
      <span class="lbl">Add another grant</span>
      <div class="grant-row">
        <div class="gf-field">
          <span class="lbl">Role</span>
          <select class="sel" bind:value={addRoleId} aria-label="New grant role">
            {#each roles as role (role.id)}
              <option value={role.id}>{role.name}</option>
            {/each}
          </select>
        </div>
        <div class="gf-field">
          <span class="lbl">Scope</span>
          <select class="sel" bind:value={addScopeKey} aria-label="New grant scope">
            <option value="" disabled>Select scope…</option>
            {#each scopeOptions as opt (opt.key)}
              <option value={opt.key}>{opt.label}</option>
            {/each}
          </select>
        </div>
        <Button variant="secondary" size="sm" disabled={!canAdd} loading={addSaving} onclick={addGrant}>
          Add
        </Button>
      </div>
      {#if addError}<p class="err-msg">{addError}</p>{/if}
    </div>
  {/if}

  {#snippet footer()}
    <Button variant="primary" onclick={onclose}>Done</Button>
  {/snippet}
</Modal>

<style>
  .grants {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .grant-row {
    display: flex;
    align-items: flex-end;
    gap: 10px;
  }
  .gf-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: 1;
    min-width: 0;
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
  .add-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }
  .err-msg {
    color: var(--error);
    font-size: 12.5px;
    margin-top: -2px;
  }
</style>
