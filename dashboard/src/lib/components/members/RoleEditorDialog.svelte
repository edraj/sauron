<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import Badge from '../ui/Badge.svelte';
  import PermissionPicker from './PermissionPicker.svelte';
  import { createRole, updateRole } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Permission, Role } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    /** null = create a new role. */
    role: Role | null;
    /** Prefill a new role from this one (name, description, permissions).
        Only takes effect while `role` is null — Copy opens the create path,
        it never overrides an edit/view. */
    copyFrom?: Role | null;
    /** How many members hold this role; shown as an impact warning on edit. */
    memberCount?: number;
    onclose: () => void;
    onsaved: (role: Role) => void;
  }

  let { open, orgId, role, copyFrom = null, memberCount = 0, onclose, onsaved }: Props = $props();

  let name = $state('');
  let description = $state('');
  let permissions = $state<Permission[]>([]);
  let saving = $state(false);
  let error = $state<string | null>(null);

  const isEdit = $derived(role !== null);
  // Presets are re-synced from rbac.rs at every API boot, so an edit would
  // revert on the next restart. Show them, never write them.
  const readOnly = $derived(role?.is_system === true);
  const title = $derived(
    readOnly
      ? `Role: ${role?.name}`
      : isEdit
        ? `Edit ${role?.name}`
        : copyFrom
          ? `New role from ${copyFrom.name}`
          : 'New role',
  );

  // Repopulate whenever the dialog opens on a different role.
  //
  // Copy opens the CREATE path (role === null) prefilled from another role, so
  // submit() still calls createRole and the server's no-escalation check still
  // applies. The Copy action is disabled when the caller lacks any of these
  // permissions, so that check cannot fail from here.
  $effect(() => {
    if (!open) return;
    const source = role ?? copyFrom;
    name = role ? source?.name ?? '' : copyFrom ? `Copy of ${copyFrom.name}` : '';
    description = source?.description ?? '';
    permissions = [...(source?.permissions ?? [])];
    error = null;
  });

  const canSubmit = $derived(!saving && !readOnly && name.trim().length > 0);

  async function submit() {
    if (!canSubmit) return;
    saving = true;
    error = null;
    const payload = {
      name: name.trim(),
      description: description.trim() || undefined,
      permissions,
    };
    try {
      const saved = role
        ? await updateRole(orgId, role.id, payload)
        : await createRole(orgId, payload);
      onsaved(saved);
      onclose();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<Modal {open} {title} onclose={onclose}>
  {#if readOnly}
    <p class="lede">
      <Badge tone="neutral" size="sm">system</Badge>
      Built-in roles cannot be edited. Create a custom role to define your own permission set.
    </p>
  {:else}
    <div class="fields">
      <Input label="Name" bind:value={name} placeholder="Support" required />
      <Input label="Description" bind:value={description} placeholder="Read + resolve issues" />
    </div>
    {#if isEdit && memberCount > 0}
      <p class="warning">
        {memberCount} {memberCount === 1 ? 'member holds' : 'members hold'} this role. Saving
        changes their access immediately.
      </p>
    {/if}
  {/if}

  <span class="lbl perms-label">Permissions</span>
  <PermissionPicker
    selected={permissions}
    disabled={readOnly}
    onchange={(next) => (permissions = next)}
  />

  {#if error}<p class="err-msg">{error}</p>{/if}

  {#snippet footer()}
    <Button variant="secondary" onclick={onclose} disabled={saving}>
      {readOnly ? 'Close' : 'Cancel'}
    </Button>
    {#if !readOnly}
      <Button variant="primary" disabled={!canSubmit} loading={saving} onclick={submit}>
        {isEdit ? 'Save changes' : 'Create role'}
      </Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .lede {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 14px;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 12px;
  }
  .warning {
    font-size: 12.5px;
    color: var(--warning);
    background: var(--warning-soft);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent);
    border-radius: var(--radius);
    padding: 8px 12px;
    margin-bottom: 14px;
  }
  .lbl {
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
    display: block;
    margin-bottom: 8px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13px;
    margin-top: 12px;
  }
</style>
