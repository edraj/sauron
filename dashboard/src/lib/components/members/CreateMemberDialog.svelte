<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import CopyButton from '../ui/CopyButton.svelte';
  import { createMember } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Role, ScopeOption } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    roles: Role[];
    scopeOptions: ScopeOption[];
    onclose: () => void;
    oncreated: () => void;
  }

  let { open, orgId, roles, scopeOptions, onclose, oncreated }: Props = $props();

  let email = $state('');
  let name = $state('');
  let roleId = $state('');
  let scopeKey = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);
  /** Set once the account exists. The dialog switches to the reveal panel. */
  let tempPassword = $state<string | null>(null);

  // Repopulate whenever the dialog opens fresh.
  $effect(() => {
    if (!open) return;
    email = '';
    name = '';
    roleId = roles[0]?.id ?? '';
    scopeKey = scopeOptions[0]?.key ?? '';
    tempPassword = null;
    error = null;
  });

  const canSubmit = $derived(
    !saving && email.trim().includes('@') && roleId !== '' && scopeKey !== '',
  );

  async function submit() {
    if (!canSubmit) return;
    const scope = scopeOptions.find((s) => s.key === scopeKey);
    if (!scope) return;
    saving = true;
    error = null;
    try {
      const result = await createMember(orgId, {
        email: email.trim(),
        name: name.trim(),
        role_id: roleId,
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
      });
      // Reveal, do not close. This is the only time this value exists.
      tempPassword = result.temp_password;
      oncreated();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<Modal {open} title={tempPassword ? 'Member created' : 'Create member'} onclose={onclose}>
  {#if tempPassword}
    <p class="lede">
      Give <strong>{email}</strong> this temporary password. They must change it the first time
      they sign in.
    </p>
    <div class="temp-password">
      <code>{tempPassword}</code>
      <CopyButton value={tempPassword} size="sm" />
    </div>
    <p class="warning">
      This is the only time it is shown. If you lose it, deactivate the account and create it
      again.
    </p>
  {:else}
    <div class="fields">
      <Input
        label="Email"
        type="email"
        bind:value={email}
        placeholder="teammate@company.com"
        required
      />
      <Input label="Name" bind:value={name} placeholder="Jane Doe" />
      <div class="gf-field">
        <span class="lbl">Role</span>
        <select class="sel" bind:value={roleId} aria-label="Role">
          {#each roles as role (role.id)}
            <option value={role.id}>{role.name}</option>
          {/each}
        </select>
      </div>
      <div class="gf-field">
        <span class="lbl">Scope</span>
        <select class="sel" bind:value={scopeKey} aria-label="Scope">
          <option value="" disabled>Select scope…</option>
          {#each scopeOptions as opt (opt.key)}
            <option value={opt.key}>{opt.label}</option>
          {/each}
        </select>
      </div>
    </div>
    {#if error}<p class="err-msg">{error}</p>{/if}
  {/if}

  {#snippet footer()}
    {#if tempPassword}
      <Button variant="primary" onclick={onclose}>Done</Button>
    {:else}
      <Button variant="ghost" onclick={onclose} disabled={saving}>Cancel</Button>
      <Button variant="primary" disabled={!canSubmit} loading={saving} onclick={submit}>
        Create member
      </Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .lede {
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
  .gf-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
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
  .temp-password {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 12px 14px;
    margin-bottom: 14px;
  }
  .temp-password code {
    font-size: 14px;
    font-weight: 620;
    letter-spacing: 0.02em;
    color: var(--text);
    word-break: break-all;
  }
  .warning {
    font-size: 12.5px;
    color: var(--warning);
    background: var(--warning-soft);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent);
    border-radius: var(--radius);
    padding: 8px 12px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13px;
    margin-top: 4px;
  }
</style>
