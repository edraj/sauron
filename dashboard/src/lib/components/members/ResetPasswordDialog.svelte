<script lang="ts">
  import { t } from '../../i18n';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import type { Member } from '../../models';

  interface Props {
    member: Member;
    action: 'reset' | 'cancel';
    busy: boolean;
    onconfirm: () => void;
    oncancel: () => void;
  }

  let { member, action, busy, onconfirm, oncancel }: Props = $props();
</script>

<Modal
  open
  title={action === 'reset' ? 'Reset this member’s password?' : 'Cancel the password reset?'}
  dismissible={!busy}
  onclose={oncancel}
>
  {#if action === 'reset'}
    <!-- The lockout is stated BEFORE the confirm button, because this sentence
         is the only warning between the admin and an account that cannot sign
         in. An admin who reads "we email them a link" and gets an unreachable
         account will not use this feature twice. -->
    <p class="lead">
      <strong>{member.email} will not be able to sign in until they use the emailed link.</strong>
    </p>
    <p>
      {t('prose.members.resetWarning')}
    </p>
  {:else}
    <p class="lead">
      {member.email} will be able to sign in with their existing password again.
    </p>
    <p>
      {t('prose.members.resetPending')}
    </p>
  {/if}

  {#snippet footer()}
    <Button variant="ghost" onclick={oncancel} disabled={busy}>{t('members.reset.neverMind')}</Button>
    <Button variant={action === 'reset' ? 'danger' : 'primary'} loading={busy} onclick={onconfirm}>
      {action === 'reset' ? 'Reset password' : 'Cancel reset'}
    </Button>
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
</style>
