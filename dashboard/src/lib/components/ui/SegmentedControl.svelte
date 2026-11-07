<!--
  The one segmented toggle (time windows, view modes, metric switches). Six
  pages hand-rolled their own in three different looks. A radiogroup: Tab
  lands on the selected segment, arrow keys move the selection.
-->
<script lang="ts">
  import { lockTip } from '../../actions/lock-tip';
  import type { LockReason } from '../../models/page-access';

  export interface SegmentOption {
    value: string;
    label: string;
    title?: string;
    disabled?: boolean;
  }

  interface Props {
    value: string;
    options: SegmentOption[];
    ariaLabel: string;
    size?: 'sm' | 'md';
    disabled?: boolean;
    /** The permission the user lacks, or `null`: focusable, explained, inert. */
    lockedReason?: LockReason | null;
    onchange?: (value: string) => void;
  }

  let {
    value = $bindable(),
    options,
    ariaLabel,
    size = 'md',
    disabled = false,
    lockedReason = null,
    onchange,
  }: Props = $props();

  // Exactly one segment is in the tab order: the selected one, or the first
  // enabled one when the value matches none (a deep link can carry a window
  // that is not a preset) — otherwise the whole control drops out of reach.
  const tabValue = $derived(
    options.some((o) => o.value === value && !o.disabled)
      ? value
      : (options.find((o) => !o.disabled)?.value ?? null),
  );

  let root: HTMLDivElement | undefined = $state();

  function pick(v: string) {
    if (disabled || lockedReason || v === value) return;
    value = v;
    onchange?.(v);
  }

  function onkeydown(e: KeyboardEvent) {
    if (lockedReason) return;
    const enabled = options.filter((o) => !o.disabled);
    const at = enabled.findIndex((o) => o.value === value);
    // Arrow direction follows reading direction, like any radiogroup.
    const rtl = root ? getComputedStyle(root).direction === 'rtl' : false;
    let step = 0;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') step = rtl && e.key === 'ArrowRight' ? -1 : 1;
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') step = rtl && e.key === 'ArrowLeft' ? 1 : -1;
    else return;
    e.preventDefault();
    // With nothing selected (a deep-linked value that is not an option),
    // forward lands on the first segment and backward on the last — not one
    // short of it, which plain modular arithmetic from -1 would give.
    const from = at === -1 ? (step > 0 ? -1 : enabled.length) : at;
    const next = enabled[(from + step + enabled.length) % enabled.length];
    if (!next) return;
    pick(next.value);
    root?.querySelector<HTMLButtonElement>(`[data-value="${CSS.escape(next.value)}"]`)?.focus();
  }
</script>

<div
  bind:this={root}
  class="seg {size}"
  class:disabled
  role="radiogroup"
  aria-label={ariaLabel}
  tabindex="-1"
  {onkeydown}
>
  {#each options as o (o.value)}
    <button
      type="button"
      role="radio"
      class:on={o.value === value}
      aria-checked={o.value === value}
      tabindex={o.value === tabValue ? 0 : -1}
      data-value={o.value}
      title={o.title}
      disabled={disabled || o.disabled}
      use:lockTip={lockedReason}
      onclick={() => pick(o.value)}
    >
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: inline-flex;
    align-items: stretch;
    height: var(--control-h);
    padding: 3px;
    gap: 2px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    flex-shrink: 0;
  }
  .seg.sm {
    height: 28px;
    padding: 2px;
  }
  .seg.disabled {
    opacity: 0.6;
  }
  button {
    display: inline-flex;
    align-items: center;
    padding: 0 10px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    transition: background-color 0.12s ease, color 0.12s ease;
  }
  .seg.sm button {
    padding: 0 8px;
  }
  button:not(:disabled):hover {
    color: var(--text);
  }
  button.on {
    background: var(--surface-3);
    color: var(--text);
  }
  button:disabled {
    cursor: not-allowed;
  }
</style>
