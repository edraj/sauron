<script lang="ts">
  import { t } from '../../i18n';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import { deleteRole } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Role } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    role: Role | null;
    /** Distinct members holding this role — the blast radius of the cascade. */
    memberCount: number;
    onclose: () => void;
    ondeleted: (role: Role, revokedGrants: number) => void;
  }

  let { open, orgId, role, memberCount, onclose, ondeleted }: Props = $props();

  let confirmName = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!open) return;
    confirmName = '';
    error = null;
  });

  // Typed confirmation, because role_grants.role_id is ON DELETE CASCADE:
  // deleting strips access from every holder at once and nothing undoes it.
  const canDelete = $derived(!busy && role !== null && confirmName.trim() === role.name);

  async function submit() {
    if (!canDelete || !role) return;
    busy = true;
    error = null;
    try {
      const { revoked_grants } = await deleteRole(orgId, role.id);
      ondeleted(role, revoked_grants);
      onclose();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal {open} title={`Delete ${role?.name ?? 'role'}`} onclose={onclose} dismissible={!busy}>
  {#if memberCount > 0}
    <p class="warning">
      {memberCount}
      {memberCount === 1 ? 'member' : 'members'} will lose this access immediately. This cannot be undone.
    </p>
  {:else}
    <p class="lede">{t('roles.deleteSafe')}</p>
  {/if}

  <Input
    label={`Type "${role?.name ?? ''}" to confirm`}
    bind:value={confirmName}
    placeholder={role?.name ?? ''}
  />

  {#if error}<p class="err-msg">{error}</p>{/if}

  {#snippet footer()}
    <Button variant="secondary" onclick={onclose} disabled={busy}>{t('common.cancel')}</Button>
    <Button variant="danger" disabled={!canDelete} loading={busy} onclick={submit}>
      {t('roles.delete')}
    </Button>
  {/snippet}
</Modal>

<style>
  .lede {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 14px;
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
  .err-msg {
    color: var(--error);
    font-size: 13px;
    margin-top: 12px;
  }
</style>
