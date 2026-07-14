<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  interface Props {
    open: boolean;
    title?: string;
    size?: 'sm' | 'md';
    onclose?: () => void;
    children: Snippet;
    footer?: Snippet;
  }

  let {
    open = $bindable(false),
    title,
    size = 'md',
    onclose,
    children,
    footer,
  }: Props = $props();

  let dialog = $state<HTMLDialogElement | null>(null);
  const titleId = `modal-${Math.random().toString(36).slice(2, 9)}`;

  // The dialog element is the single source of truth for open/closed; this
  // effect keeps the native <dialog> in sync with the `open` prop.
  $effect(() => {
    const el = dialog;
    if (!el) return;
    if (open && !el.open) {
      el.showModal();
      // Focus the first field (or the safe default action), not the close X.
      const focusable = el.querySelector<HTMLElement>(
        'input, textarea, select, [href], button:not(.m-close)',
      );
      focusable?.focus();
    } else if (!open && el.open) {
      el.close();
    }
  });

  function requestClose() {
    open = false;
    onclose?.();
  }

  // Escape fires `cancel` then would auto-close; intercept so we own the path.
  function onCancel(e: Event) {
    e.preventDefault();
    requestClose();
  }

  // A click landing on ::backdrop reports the <dialog> itself as the target.
  function onBackdropClick(e: MouseEvent) {
    if (e.target === dialog) requestClose();
  }
</script>

<dialog
  bind:this={dialog}
  class="modal {size}"
  aria-labelledby={title ? titleId : undefined}
  oncancel={onCancel}
  onclick={onBackdropClick}
>
  <div class="panel">
    <header class="m-head">
      {#if title}<h2 id={titleId} class="m-title">{title}</h2>{/if}
      <button class="m-close" type="button" aria-label="Close" onclick={requestClose}>
        <Icon name="x" size={16} />
      </button>
    </header>
    <div class="m-body">
      {@render children()}
    </div>
    {#if footer}
      <footer class="m-foot">{@render footer()}</footer>
    {/if}
  </div>
</dialog>

<style>
  .modal {
    /* The app's global `* { margin: 0 }` reset kills the UA `margin: auto`
       that centers a modal <dialog>, so center it explicitly. */
    position: fixed;
    inset: 0;
    margin: auto;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-lg);
    width: min(480px, calc(100vw - 32px));
    max-width: calc(100vw - 32px);
    overflow: hidden;
  }
  .modal.sm {
    width: min(400px, calc(100vw - 32px));
  }
  .modal::backdrop {
    background: rgba(8, 10, 14, 0.55);
    backdrop-filter: blur(2px);
  }
  .modal[open] {
    animation: modal-in 0.18s cubic-bezier(0.2, 0.9, 0.3, 1);
  }
  .modal[open]::backdrop {
    animation: backdrop-in 0.18s ease;
  }
  @keyframes modal-in {
    from {
      opacity: 0;
      transform: translateY(6px) scale(0.98);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @keyframes backdrop-in {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }

  .panel {
    display: flex;
    flex-direction: column;
    max-height: min(85vh, 640px);
  }
  .m-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 16px 18px;
    border-bottom: 1px solid var(--border);
  }
  .m-title {
    font-size: 15px;
    font-weight: 620;
    color: var(--text);
  }
  .m-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--text-faint);
    padding: 4px;
    border-radius: var(--radius-sm);
    cursor: pointer;
    flex-shrink: 0;
  }
  .m-close:hover {
    color: var(--text);
    background: var(--surface-3);
  }
  .m-body {
    padding: 18px;
    overflow-y: auto;
  }
  .m-foot {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 14px 18px;
    border-top: 1px solid var(--border);
  }
</style>
