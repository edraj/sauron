<script lang="ts">
  import type { Snippet } from 'svelte';
  import Spinner from './Spinner.svelte';
  import Icon from './Icon.svelte';
  import type { Permission } from '../../models';
  import { lockTitle } from '../../models/page-access';

  type Variant = 'primary' | 'secondary' | 'ghost' | 'danger' | 'subtle';
  type Size = 'sm' | 'md' | 'lg';

  interface Props {
    variant?: Variant;
    size?: Size;
    type?: 'button' | 'submit' | 'reset';
    href?: string;
    disabled?: boolean;
    loading?: boolean;
    fullWidth?: boolean;
    title?: string;
    /**
     * The permission the user is missing, or `null` if they may act — the
     * shape `lockedBy()` returns, so a call site never needs a ternary.
     *
     * Non-null disables the button, prefixes a lock glyph, and sets a title
     * naming the permission. Prefer this over hiding the control: a user who
     * cannot see a capability cannot learn it exists or ask for it, and a
     * plain `disabled` never explains itself.
     */
    lockedReason?: Permission | null;
    onclick?: (event: MouseEvent) => void;
    children: Snippet;
  }

  let {
    variant = 'secondary',
    size = 'md',
    type = 'button',
    href,
    disabled = false,
    loading = false,
    fullWidth = false,
    title,
    lockedReason = null,
    onclick,
    children,
  }: Props = $props();

  // Folding the lock into `isDisabled` also routes a locked `href` button
  // through the <button> branch below, which matters: an <a> cannot be
  // disabled, so a locked link would otherwise stay fully clickable.
  const isDisabled = $derived(disabled || loading || lockedReason !== null);
  // The lock reason outranks a caller-supplied title — why you cannot act
  // matters more than whatever hint it replaces.
  const resolvedTitle = $derived(lockedReason ? lockTitle(lockedReason) : title);
</script>

{#if href && !isDisabled}
  <a
    class="btn {variant} {size}"
    class:full={fullWidth}
    {href}
    title={resolvedTitle}
    {onclick}
  >
    {@render children()}
  </a>
{:else}
  <button
    class="btn {variant} {size}"
    class:full={fullWidth}
    class:is-loading={loading}
    {type}
    title={resolvedTitle}
    disabled={isDisabled}
    onclick={onclick}
  >
    {#if loading}
      <span class="spin"><Spinner size={size === 'sm' ? 14 : 16} /></span>
    {/if}
    <span class="label" class:hidden={loading}>
      {#if lockedReason}<span class="lock" aria-hidden="true"><Icon name="lock" size={13} /></span
        >{/if}{@render children()}
    </span>
  </button>
{/if}

<style>
  .btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    border: 1px solid transparent;
    border-radius: var(--radius);
    font-weight: 560;
    line-height: 1;
    white-space: nowrap;
    text-decoration: none;
    transition: background-color 0.14s ease, border-color 0.14s ease, color 0.14s ease,
      transform 0.05s ease, box-shadow 0.14s ease;
    user-select: none;
  }

  .btn.sm {
    padding: 6px 11px;
    font-size: 12.5px;
    border-radius: var(--radius-sm);
  }
  .btn.md {
    padding: 9px 15px;
    font-size: 13.5px;
  }
  .btn.lg {
    padding: 12px 20px;
    font-size: 15px;
  }

  .btn.full {
    width: 100%;
  }

  .btn:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .btn:not(:disabled):active {
    transform: translateY(1px);
  }

  /* primary */
  .btn.primary {
    background: var(--primary);
    color: var(--primary-contrast);
    box-shadow: var(--shadow-sm);
  }
  .btn.primary:not(:disabled):hover {
    background: var(--primary-hover);
  }

  /* secondary */
  .btn.secondary {
    background: var(--surface-2);
    border-color: var(--border-strong);
    color: var(--text);
  }
  .btn.secondary:not(:disabled):hover {
    background: var(--surface-3);
    border-color: var(--text-faint);
  }

  /* subtle */
  .btn.subtle {
    background: var(--surface-2);
    color: var(--text-muted);
  }
  .btn.subtle:not(:disabled):hover {
    background: var(--surface-3);
    color: var(--text);
  }

  /* ghost */
  .btn.ghost {
    background: transparent;
    color: var(--text-muted);
  }
  .btn.ghost:not(:disabled):hover {
    background: var(--surface-2);
    color: var(--text);
  }

  /* danger */
  .btn.danger {
    background: var(--error-soft);
    border-color: var(--error);
    color: var(--error);
  }
  .btn.danger:not(:disabled):hover {
    background: var(--error);
    color: #fff;
  }

  .spin {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
  }

  .label.hidden {
    visibility: hidden;
  }

  .lock {
    display: inline-flex;
    vertical-align: -2px;
    margin-right: 5px;
  }
</style>
