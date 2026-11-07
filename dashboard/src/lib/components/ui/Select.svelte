<!--
  The one dropdown. A native <select> (keyboard, screen readers and mobile
  pickers for free) inside exactly `Input`'s shell, so a select beside an input
  is the same height, border, radius and label. Pages used to restyle <select>
  six different ways, or not at all.
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import { lockTip } from '../../actions/lock-tip';
  import type { LockReason } from '../../models/page-access';

  export interface SelectOption {
    value: string;
    label: string;
    disabled?: boolean;
  }

  interface Props {
    value: string;
    options: SelectOption[];
    label?: string;
    hint?: string;
    error?: string;
    id?: string;
    name?: string;
    disabled?: boolean;
    /** `sm` is 28px, for dense rows; `md` is `--control-h`. */
    size?: 'sm' | 'md';
    /** Accessible name when there is no visible `label`. */
    ariaLabel?: string;
    title?: string;
    /**
     * The permission the user lacks, or `null`. Like `Button`'s: the select
     * stays focusable and explains itself through `lockTip`, but cannot be
     * opened or changed — `disabled` would hide it from keyboard users, who
     * most need the reason.
     */
    lockedReason?: LockReason | null;
    onchange?: (value: string) => void;
  }

  let {
    value = $bindable(),
    options,
    label,
    hint,
    error,
    id,
    name,
    disabled = false,
    size = 'md',
    ariaLabel,
    title,
    lockedReason = null,
    onchange,
  }: Props = $props();

  // Keys that would change a native select's value without opening it.
  const CHANGING_KEYS = new Set(['ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown', ' ', 'Enter']);

  const generatedId = `s-${Math.random().toString(36).slice(2, 9)}`;
  const fieldId = $derived(id ?? generatedId);
</script>

<div class="field">
  {#if label}<label class="lbl" for={fieldId}>{label}</label>{/if}
  <div class="control {size}" class:has-error={!!error} class:disabled>
    <select
      id={fieldId}
      {name}
      {disabled}
      title={lockedReason ? undefined : title}
      aria-label={label ? undefined : ariaLabel}
      use:lockTip={lockedReason}
      {value}
      onmousedown={(e) => {
        if (lockedReason) e.preventDefault();
      }}
      onkeydown={(e) => {
        if (lockedReason && (CHANGING_KEYS.has(e.key) || e.key.length === 1)) e.preventDefault();
      }}
      onchange={(e) => {
        const el = e.currentTarget as HTMLSelectElement;
        // The backstop for the two guards above: a phone's native picker opens
        // on a tap with no cancellable mousedown, so a locked select can still
        // be changed. Put the old value back and tell no one.
        if (lockedReason) {
          el.value = value;
          return;
        }
        value = el.value;
        onchange?.(value);
      }}
    >
      {#each options as o (o.value)}
        <option value={o.value} disabled={o.disabled}>{o.label}</option>
      {/each}
    </select>
    <span class="chev" aria-hidden="true"><Icon name={lockedReason ? 'lock' : 'chevron-down'} size={lockedReason ? 12 : 14} /></span>
  </div>
  {#if error}
    <span class="msg err">{error}</span>
  {:else if hint}
    <span class="msg hint">{hint}</span>
  {/if}
</div>

<style>
  .field {
    display: inline-flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .lbl {
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    color: var(--text-muted);
  }
  .control {
    position: relative;
    display: flex;
    align-items: center;
    height: var(--control-h);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    transition: border-color 0.12s ease, box-shadow 0.12s ease;
  }
  .control.sm {
    height: 28px;
  }
  .control:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  .control.has-error {
    border-color: var(--error);
  }
  .control.disabled {
    opacity: 0.6;
  }
  select {
    appearance: none;
    width: 100%;
    height: 100%;
    padding-block: 0;
    padding-inline: 10px 30px;
    background: transparent;
    border: none;
    outline: none;
    color: var(--text);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    cursor: pointer;
  }
  .control.sm select {
    font-size: var(--text-xs);
    padding-inline: 8px 26px;
  }
  select:disabled {
    cursor: not-allowed;
  }
  .chev {
    position: absolute;
    inset-inline-end: 9px;
    display: inline-flex;
    color: var(--text-faint);
    pointer-events: none;
  }
  .msg {
    font-size: var(--text-xs);
  }
  .msg.err {
    color: var(--error);
  }
  .msg.hint {
    color: var(--text-faint);
  }
</style>
