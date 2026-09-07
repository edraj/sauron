<script lang="ts">
  import { t } from '../../i18n';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import { newEmailProblem } from '../../models/email-change';
  import type { Member } from '../../models';

  interface Props {
    member: Member;
    /** `request` opens a pending change; `withdraw` cancels the one shown on
        the row. ONE dialog rather than two, mirroring `ResetPasswordDialog`:
        the two actions are never both available for a member. */
    action: 'request' | 'withdraw';
    busy: boolean;
    onconfirm: (newEmail: string) => void;
    oncancel: () => void;
  }

  let { member, action, busy, onconfirm, oncancel }: Props = $props();

  let newEmail = $state('');

  // Mirrors the server's own three refusals, so the dialog never submits
  // something that comes straight back as a 400 or 409.
  const problem = $derived(newEmailProblem(newEmail, member.email));
  const canSubmit = $derived(problem === null && !busy);

  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    onconfirm(newEmail.trim().toLowerCase());
  }
</script>

<Modal
  open
  title={action === 'request'
    ? t('members.changeEmail.title')
    : t('members.withdrawEmailChange.title')}
  dismissible={!busy}
  onclose={oncancel}
>
  {#if action === 'request'}
    <!-- Stated BEFORE the input, because an admin who reads only the button
         label will assume this takes effect immediately, then be surprised
         when the member keeps signing in with the old address — and will
         "fix" it by asking again, mailing the member a second warning. -->
    <p class="lead">{t('prose.members.changeEmailWarning')}</p>
    <form onsubmit={submit}>
      <Input
        type="email"
        bind:value={newEmail}
        label={t('members.changeEmail.newAddress')}
        placeholder={member.email}
        disabled={busy}
      />
    </form>
    {#if problem === 'unchanged'}
      <p class="problem">{t('members.changeEmail.unchanged')}</p>
    {:else if problem === 'malformed' && newEmail.trim().length > 0}
      <p class="problem">{t('members.changeEmail.malformed')}</p>
    {/if}
  {:else if member.pending_email_change}
    <p class="lead">
      {t('members.changeEmail.pendingTo')}
      <strong>{member.pending_email_change.new_email}</strong>
    </p>
    <p>{t('prose.members.withdrawEmailChange')}</p>
  {/if}

  {#snippet footer()}
    <Button variant="ghost" onclick={oncancel} disabled={busy}
      >{t('members.reset.neverMind')}</Button
    >
    {#if action === 'request'}
      <Button variant="primary" loading={busy} disabled={!canSubmit} onclick={() => onconfirm(newEmail.trim().toLowerCase())}>
        {t('members.changeEmail.submit')}
      </Button>
    {:else}
      <Button variant="danger" loading={busy} onclick={() => onconfirm('')}>
        {t('members.withdrawEmailChange.submit')}
      </Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .lead {
    font-size: 14px;
    line-height: 1.5;
    margin-bottom: 10px;
  }
  p {
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--text-muted);
  }
  form {
    margin-top: 4px;
  }
  .problem {
    margin-top: 6px;
    color: var(--danger, #c0392b);
  }
</style>
