<script lang="ts">
  import Icon, { type IconName } from './Icon.svelte';
  import { toastStore } from '../../stores/toast.svelte';

  const icons: Record<'success' | 'error' | 'info', IconName> = {
    success: 'circle-check',
    error: 'circle-x',
    info: 'info',
  };
</script>

<div class="toast-host" aria-live="polite">
  {#each toastStore.items as toast (toast.id)}
    <div class="toast {toast.kind}" role="status">
      <span class="ico"><Icon name={icons[toast.kind]} size={16} /></span>
      <span class="msg">{toast.message}</span>
      <button class="close" aria-label="Dismiss" onclick={() => toastStore.dismiss(toast.id)}>
        <Icon name="x" size={14} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toast-host {
    position: fixed;
    bottom: 20px;
    right: 20px;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-width: min(380px, calc(100vw - 40px));
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 12px 14px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-left-width: 3px;
    border-radius: var(--radius);
    box-shadow: var(--shadow-lg);
    animation: toast-in 0.22s cubic-bezier(0.2, 0.9, 0.3, 1);
  }
  .toast.success {
    border-left-color: var(--success);
  }
  .toast.error {
    border-left-color: var(--error);
  }
  .toast.info {
    border-left-color: var(--info);
  }
  .ico {
    font-weight: 700;
    line-height: 1.4;
  }
  .toast.success .ico {
    color: var(--success);
  }
  .toast.error .ico {
    color: var(--error);
  }
  .toast.info .ico {
    color: var(--info);
  }
  .msg {
    flex: 1;
    font-size: 13px;
    color: var(--text);
  }
  .close {
    background: none;
    border: none;
    color: var(--text-faint);
    font-size: 11px;
    padding: 2px;
    line-height: 1;
  }
  .close:hover {
    color: var(--text);
  }
  @keyframes toast-in {
    from {
      opacity: 0;
      transform: translateX(16px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
</style>
