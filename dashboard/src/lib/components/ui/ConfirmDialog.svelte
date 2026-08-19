<script lang="ts">
  import { t } from '../../i18n';
  import Modal from './Modal.svelte';
  import Button from './Button.svelte';

  interface Props {
    open: boolean;
    title: string;
    message: string;
    confirmLabel?: string;
    cancelLabel?: string;
    danger?: boolean;
    loading?: boolean;
    onconfirm: () => void;
    oncancel: () => void;
  }

  let {
    open = $bindable(false),
    title,
    message,
    confirmLabel,
    cancelLabel,
    danger = false,
    loading = false,
    onconfirm,
    oncancel,
  }: Props = $props();

  // Defaulted through `$derived` rather than in the destructuring pattern,
  // so switching language re-renders the buttons instead of leaving whichever
  // locale was active when the dialog first mounted.
  const confirmText = $derived(confirmLabel ?? t('common.confirm'));
  const cancelText = $derived(cancelLabel ?? t('common.cancel'));
</script>

<Modal bind:open size="sm" {title} onclose={oncancel}>
  <p class="msg">{message}</p>
  {#snippet footer()}
    <Button variant="secondary" onclick={oncancel} disabled={loading}>{cancelText}</Button>
    <Button variant={danger ? 'danger' : 'primary'} onclick={onconfirm} {loading}>{confirmText}</Button>
  {/snippet}
</Modal>

<style>
  .msg {
    font-size: 13.5px;
    color: var(--text-muted);
    line-height: 1.55;
    margin: 0;
  }
</style>
