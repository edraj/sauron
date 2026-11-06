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
  title={action === 'reset' ? t('members.reset.title') : t('members.reset.cancelTitle')}
  dismissible={!busy}
  onclose={oncancel}
>
  {#if action === 'reset'}
    <!-- The lockout is stated BEFORE the confirm button, because this sentence
         is the only warning between the admin and an account that cannot sign
         in. An admin who reads "we email them a link" and gets an unreachable
         account will not use this feature twice. -->
    <p class="lead">
      <strong>{t('members.reset.lockout', { email: member.email })}</strong>
    </p>
    <p>
      {t('prose.members.resetWarning')}
    </p>
  {:else}
    <p class="lead">
      {t('members.reset.cancelLead', { email: member.email })}
    </p>
    <p>
      {t('prose.members.resetPending')}
    </p>
  {/if}

  {#snippet footer()}
    <Button variant="secondary" onclick={oncancel} disabled={busy}>{t('members.reset.neverMind')}</Button>
    <Button variant={action === 'reset' ? 'danger' : 'primary'} loading={busy} onclick={onconfirm}>
      {action === 'reset' ? t('members.reset.action') : t('members.reset.cancelAction')}
    </Button>
  {/snippet}
</Modal>

<style>
  .lead {
    font-size: var(--text-sm);
    line-height: 1.5;
    margin-bottom: 10px;
  }
  p {
    font-size: var(--text-sm);
    line-height: 1.55;
    color: var(--text-muted);
  }
</style>
