<script lang="ts">
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
    confirmLabel = 'Confirm',
    cancelLabel = 'Cancel',
    danger = false,
    loading = false,
    onconfirm,
    oncancel,
  }: Props = $props();
</script>

<Modal bind:open size="sm" {title} onclose={oncancel}>
  <p class="msg">{message}</p>
  {#snippet footer()}
    <Button variant="secondary" onclick={oncancel} disabled={loading}>{cancelLabel}</Button>
    <Button variant={danger ? 'danger' : 'primary'} onclick={onconfirm} {loading}>{confirmLabel}</Button>
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
