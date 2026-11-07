<script lang="ts">
  import type { Snippet } from 'svelte';
  import Spinner from './Spinner.svelte';
  import Icon from './Icon.svelte';
  import { lockTip } from '../../actions/lock-tip';
  import type { LockReason } from '../../models/page-access';

  type Variant = 'primary' | 'secondary' | 'ghost' | 'danger';
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
     * Non-null neutralises the button, prefixes a lock glyph, and describes it
     * with a tooltip naming the permission. Prefer this over hiding the
     * control: a user who cannot see a capability cannot learn it exists or ask
     * for it, and a plain `disabled` never explains itself.
     *
     * Renders `aria-disabled`, NOT `disabled`. `disabled` removes the control
     * from the tab order, so the people who most need the explanation —
     * keyboard and screen reader users — could never focus it to receive one.
     * `aria-disabled` announces the state and keeps it reachable, at the cost
     * of suppressing the click and any form submission by hand below, since
     * the attribute does neither on its own.
     */
    lockedReason?: LockReason | null;
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

  const isLocked = $derived(lockedReason !== null);
  // `disabled` and `loading` stay REAL `disabled`: both are transient states of
  // a control the user is already allowed to use, so removing them from the tab
  // order costs nothing. A lock is a permanent property they may need to read.
  const isDisabled = $derived(disabled || loading);
  // Either one routes an `href` button through the <button> branch below, which
  // matters: an <a> can be neither disabled nor meaningfully aria-disabled, so
  // a locked link would otherwise stay fully clickable.
  const isInert = $derived(isDisabled || isLocked);
  // The lock reason outranks a caller-supplied title — why you cannot act
  // matters more than whatever hint it replaces, and `lockTip` delivers it in a
  // form a keyboard user can actually reach.
  const resolvedTitle = $derived(lockedReason ? undefined : title);
</script>

{#if href && !isInert}
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
    class:locked={isLocked}
    {type}
    title={resolvedTitle}
    disabled={isDisabled}
    use:lockTip={lockedReason}
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
  /* Four variants, one job each: `primary` for the single main action in a
     view, `secondary` for everything else, `ghost` for tertiary and icon-only
     controls, `danger` for anything irreversible. */
  .btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border: 1px solid transparent;
    border-radius: var(--radius);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    line-height: 1;
    white-space: nowrap;
    text-decoration: none;
    transition: background-color 0.12s ease, border-color 0.12s ease, color 0.12s ease,
      transform 0.05s ease;
    user-select: none;
  }

  .btn.sm {
    min-height: 28px;
    padding: 0 10px;
    font-size: var(--text-xs);
  }
  /* `min-height` so a md button lines up with the `--control-h` controls it
     usually sits beside (Export next to Refresh, Save next to an input). */
  .btn.md {
    min-height: var(--control-h);
    padding: 0 12px;
  }
  .btn.lg {
    min-height: 40px;
    padding: 0 16px;
  }

  .btn.full {
    width: 100%;
  }

  /* Pressed: a one-pixel settle, so a click registers before any result. */
  .btn:not(:disabled):not(.locked):active {
    transform: translateY(1px);
  }

  .btn:disabled,
  .btn.locked {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .btn.primary {
    background: var(--primary-solid);
    border-color: var(--primary-solid);
    color: var(--primary-contrast);
  }
  .btn.primary:not(:disabled):not(.locked):hover {
    background: var(--primary-solid-hover);
    border-color: var(--primary-solid-hover);
  }
  .btn.primary:not(:disabled):not(.locked):active {
    background: var(--primary-active);
    border-color: var(--primary-active);
  }

  .btn.secondary {
    background: var(--surface);
    border-color: var(--border-strong);
    color: var(--text);
  }
  .btn.secondary:not(:disabled):not(.locked):hover {
    background: var(--surface-2);
  }

  .btn.ghost {
    background: transparent;
    color: var(--text-muted);
  }
  .btn.ghost:not(:disabled):not(.locked):hover {
    background: var(--surface-2);
    color: var(--text);
  }

  .btn.danger {
    background: var(--error-solid);
    border-color: var(--error-solid);
    color: var(--primary-contrast);
  }
  .btn.danger:not(:disabled):not(.locked):hover {
    background: color-mix(in srgb, var(--error-solid) 88%, black);
    border-color: color-mix(in srgb, var(--error-solid) 88%, black);
  }

  .spin {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
  }
  .label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .label.hidden {
    visibility: hidden;
  }
  .lock {
    display: inline-flex;
    margin-inline-end: 2px;
  }
</style>
