<!--
  A breadcrumb segment that opens a switcher menu. Used for the org / project /
  app selectors in the Topbar. The trigger looks like a static label but is
  clickable — even with a single option — and the popup lists every option (the
  current one checked) plus an optional "+ New …" action.

  The menu is portalled to <body> because the Topbar clips its children
  (`overflow: hidden`) and its `backdrop-filter` would otherwise anchor a
  fixed-positioned menu to the bar instead of the viewport.
-->
<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import type { IconName } from '../ui/Icon.svelte';

  interface Item {
    id: string;
    name: string;
    icon?: IconName;
  }

  interface Props {
    /** Small uppercase kind chip shown in the trigger (e.g. "Org"). */
    label?: string;
    /** Icon shown in the trigger next to the current selection's name. */
    triggerIcon?: IconName;
    items: Item[];
    currentId: string | null;
    onSelect: (id: string) => void;
    /** Trailing create action label (omit, or omit onCreate, to hide it). */
    createLabel?: string;
    onCreate?: () => void;
    ariaLabel: string;
  }

  let {
    label,
    triggerIcon,
    items,
    currentId,
    onSelect,
    createLabel,
    onCreate,
    ariaLabel,
  }: Props = $props();

  let open = $state(false);
  let triggerEl = $state<HTMLButtonElement | null>(null);
  let menuEl = $state<HTMLDivElement | null>(null);
  let pos = $state({ top: 0, left: 0, minWidth: 0 });

  const current = $derived(items.find((i) => i.id === currentId) ?? null);

  function reposition() {
    if (!triggerEl) return;
    const r = triggerEl.getBoundingClientRect();
    pos = { top: r.bottom + 6, left: r.left, minWidth: r.width };
  }

  function toggle() {
    if (open) {
      open = false;
    } else {
      reposition();
      open = true;
    }
  }

  function choose(id: string) {
    open = false;
    if (id !== currentId) onSelect(id);
  }

  function create() {
    open = false;
    onCreate?.();
  }

  // Roving focus within the open menu (Escape is handled document-wide below,
  // so it closes even while focus sits on the trigger rather than an item).
  function onMenuKeydown(e: KeyboardEvent) {
    if (!menuEl) return;
    const focusables = Array.from(
      menuEl.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]'),
    );
    if (focusables.length === 0) return;
    const idx = focusables.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      focusables[(idx + 1 + focusables.length) % focusables.length].focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      focusables[(idx - 1 + focusables.length) % focusables.length].focus();
    } else if (e.key === 'Home') {
      e.preventDefault();
      focusables[0].focus();
    } else if (e.key === 'End') {
      e.preventDefault();
      focusables[focusables.length - 1].focus();
    }
  }

  // While open: close on outside pointerdown, follow the trigger on scroll/resize.
  $effect(() => {
    if (!open) return;
    function onDocPointer(e: PointerEvent) {
      const t = e.target as Node;
      if (triggerEl?.contains(t) || menuEl?.contains(t)) return;
      open = false;
    }
    function onScrollResize() {
      reposition();
    }
    function onDocKeydown(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        open = false;
        triggerEl?.focus();
      }
    }
    document.addEventListener('pointerdown', onDocPointer, true);
    document.addEventListener('keydown', onDocKeydown);
    window.addEventListener('resize', onScrollResize);
    window.addEventListener('scroll', onScrollResize, true);
    return () => {
      document.removeEventListener('pointerdown', onDocPointer, true);
      document.removeEventListener('keydown', onDocKeydown);
      window.removeEventListener('resize', onScrollResize);
      window.removeEventListener('scroll', onScrollResize, true);
    };
  });

  // Move focus into the menu when it opens (current item, else first).
  $effect(() => {
    if (open && menuEl) {
      const target =
        menuEl.querySelector<HTMLButtonElement>('.item.active') ??
        menuEl.querySelector<HTMLButtonElement>('button[role="menuitem"]');
      target?.focus();
    }
  });

  /** Relocate the node to <body> so it escapes the Topbar's clipping context. */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      },
    };
  }
</script>

<button
  class="trigger"
  class:open
  bind:this={triggerEl}
  onclick={toggle}
  aria-haspopup="menu"
  aria-expanded={open}
  aria-label={ariaLabel}
>
  {#if label}<span class="kind">{label}</span>{/if}
  {#if triggerIcon}<span class="t-icon" aria-hidden="true"><Icon name={triggerIcon} size={15} /></span>{/if}
  <span class="name">{current?.name ?? '—'}</span>
  <span class="chev"><Icon name="chevron-down" size={14} /></span>
</button>

{#if open}
  <div
    class="menu"
    role="menu"
    tabindex="-1"
    aria-label={ariaLabel}
    bind:this={menuEl}
    use:portal
    style="top:{pos.top}px; left:{pos.left}px; min-width:{pos.minWidth}px"
    onkeydown={onMenuKeydown}
  >
    <ul class="items">
      {#each items as item (item.id)}
        <li>
          <button
            type="button"
            class="item"
            class:active={item.id === currentId}
            role="menuitem"
            onclick={() => choose(item.id)}
          >
            {#if item.icon}<span class="i-icon" aria-hidden="true"><Icon name={item.icon} size={15} /></span>{/if}
            <span class="i-name">{item.name}</span>
            {#if item.id === currentId}<span class="i-check" aria-hidden="true"><Icon name="check" size={14} /></span>{/if}
          </button>
        </li>
      {/each}
    </ul>
    {#if createLabel && onCreate}
      <div class="sep"></div>
      <button type="button" class="create" role="menuitem" onclick={create}>
        <span class="plus" aria-hidden="true">+</span>{createLabel}
      </button>
    {/if}
  </div>
{/if}

<style>
  .trigger {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px 6px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    min-width: 0;
    cursor: pointer;
    transition: background 0.13s ease, border-color 0.13s ease;
  }
  .trigger:hover,
  .trigger.open {
    background: var(--surface-3);
    border-color: var(--border-strong);
  }
  .kind {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .t-icon {
    display: inline-flex;
    line-height: 1;
  }
  .name {
    font-weight: 600;
    font-size: 13.5px;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 180px;
  }
  .chev {
    display: inline-flex;
    color: var(--text-faint);
    transition: transform 0.15s ease;
  }
  .trigger.open .chev {
    transform: rotate(180deg);
  }

  /* Portalled — lives under <body>, so it can't inherit topbar styles. */
  .menu {
    position: fixed;
    z-index: 100;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    padding: 4px;
    max-height: min(320px, calc(100vh - 80px));
    overflow-y: auto;
    max-width: 280px;
  }
  .items {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 9px;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 13.5px;
    font-weight: 540;
    text-align: left;
    cursor: pointer;
    transition: background 0.1s ease, color 0.1s ease;
  }
  .item:hover,
  .item:focus-visible {
    background: var(--surface-2);
    color: var(--text);
    outline: none;
  }
  .item.active {
    color: var(--text);
  }
  .i-icon {
    display: inline-flex;
    line-height: 1;
    color: var(--text-faint);
  }
  .i-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .i-check {
    display: inline-flex;
    color: var(--primary);
  }
  .sep {
    height: 1px;
    background: var(--border);
    margin: 4px 2px;
  }
  .create {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 7px 9px;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 13px;
    font-weight: 540;
    text-align: left;
    cursor: pointer;
    transition: background 0.1s ease, color 0.1s ease;
  }
  .create:hover,
  .create:focus-visible {
    background: var(--surface-2);
    color: var(--text);
    outline: none;
  }
  .plus {
    font-size: 15px;
    line-height: 1;
    color: var(--text-faint);
  }

  @media (max-width: 860px) {
    .kind {
      display: none;
    }
    .trigger {
      padding: 6px 8px 6px 10px;
    }
  }
  @media (max-width: 640px) {
    .name {
      max-width: 110px;
    }
  }
</style>
