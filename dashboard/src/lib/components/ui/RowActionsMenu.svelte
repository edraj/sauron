<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    /** Accessible name for the trigger, e.g. `Actions for ada@example.com`. A
        table of twenty identical "More" buttons is unusable with a screen
        reader. */
    label: string;
    /** Menu items. Receives a `close` callback: every item must call it, or the
        panel stays open over the dialog the item just opened. */
    children: Snippet<[() => void]>;
  }

  let { label, children }: Props = $props();

  let open = $state(false);
  let trigger = $state<HTMLButtonElement | null>(null);
  let panel = $state<HTMLDivElement | null>(null);
  // Viewport coordinates for the panel, measured off the trigger at open time.
  let pos = $state<{ top: number; right: number } | null>(null);

  /** Close without moving focus. */
  function dismiss(): void {
    open = false;
  }

  function close(): void {
    if (!open) return;
    open = false;
    // Focus returns to the trigger. Without this a keyboard user who presses
    // Escape is dropped at the top of the document, twenty rows away from where
    // they were.
    trigger?.focus();
  }

  function toggle(): void {
    if (open) {
      close();
      return;
    }
    // MEASURED, not `position: absolute`. The members table sits inside
    // `.table-scroll { overflow-x: auto }`, and a non-visible overflow on one
    // axis promotes the other to `auto` — so an absolutely positioned panel on
    // the last row is clipped by the scroll container and only reachable by
    // scrolling inside the table. Verified: the panel overhung the container by
    // 115px. Fixed coordinates escape that clip.
    const rect = trigger?.getBoundingClientRect();
    if (rect) pos = { top: rect.bottom + 4, right: Math.max(0, window.innerWidth - rect.right) };
    open = true;
  }

  function onWindowPointerDown(event: PointerEvent) {
    if (!open) return;
    const target = event.target as Node | null;
    if (target && (trigger?.contains(target) || panel?.contains(target))) return;
    // No focus() on this path: the click has already moved focus somewhere
    // deliberate, and yanking it back would fight the user.
    dismiss();
  }

  function onWindowKeyDown(event: KeyboardEvent) {
    if (open && event.key === 'Escape') close();
  }

  /** Anything that moves the trigger strands the panel, which no longer tracks
      it. Capture phase, because scroll does not bubble and the scroller here is
      the table wrapper, not the window. */
  function onAnyScrollOrResize() {
    if (open) dismiss();
  }
</script>

<svelte:window
  onpointerdown={onWindowPointerDown}
  onkeydown={onWindowKeyDown}
  onscrollcapture={onAnyScrollOrResize}
  onresize={onAnyScrollOrResize}
/>

<div class="ram">
  <button
    type="button"
    class="ram-trigger"
    bind:this={trigger}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label={label}
    onclick={toggle}
  >
    <span aria-hidden="true">⋯</span>
  </button>
  {#if open && pos}
    <div
      class="ram-panel"
      role="menu"
      bind:this={panel}
      style="top: {pos.top}px; right: {pos.right}px;"
    >
      {@render children(close)}
    </div>
  {/if}
</div>

<style>
  .ram {
    display: inline-flex;
  }
  .ram-trigger {
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 16px;
    line-height: 1;
    padding: 4px 8px;
    cursor: pointer;
  }
  .ram-trigger:hover,
  .ram-trigger[aria-expanded='true'] {
    color: var(--text);
    border-color: var(--border);
  }
  .ram-panel {
    /* `fixed`, and `top`/`right` come from the inline style — see toggle(). */
    position: fixed;
    z-index: 20;
    min-width: 190px;
    padding: 4px;
    display: flex;
    flex-direction: column;
    /* --surface-2, not --bg: the panel floats over a Card (--surface), and the
       page background reads as a hole punched in the card rather than a layer
       above it. Both tokens are redefined by [data-theme='light']. */
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.28);
    text-align: left;
  }
  /* The parent <td> is `white-space: nowrap` for the trigger's sake; items
     inside the panel must not inherit that or a long label is clipped. */
  .ram-panel :global(.ram-item) {
    display: block;
    width: 100%;
    padding: 7px 10px;
    background: none;
    border: none;
    border-radius: calc(var(--radius) - 2px);
    color: var(--text);
    font-size: 13px;
    text-align: left;
    white-space: normal;
    cursor: pointer;
  }
  /* A white-tinted overlay would be invisible in the light theme, which ships
     and is user-selectable (themeStore). --surface-3 is the house hover token
     and is defined in both themes. */
  .ram-panel :global(.ram-item:hover:not(:disabled)) {
    background: var(--surface-3);
  }
  .ram-panel :global(.ram-item:disabled) {
    cursor: not-allowed;
    opacity: 0.5;
  }
  .ram-panel :global(.ram-item.danger) {
    color: var(--error);
  }
  /* Lock glyph for a menu item the caller has disabled on a missing
     permission. Items are caller-authored markup, so this is the shared hook
     they opt into rather than a prop this component owns. */
  .ram-panel :global(.ram-lock) {
    display: inline-flex;
    vertical-align: -2px;
    margin-right: 6px;
    color: var(--text-faint);
  }
</style>
