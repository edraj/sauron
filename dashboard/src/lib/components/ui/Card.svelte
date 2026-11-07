<script lang="ts" module>
  // `aria-controls` needs a unique id per card; a module counter is enough —
  // nothing server-renders these, so there is no hydration id to agree with.
  let nextId = 0;
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  /**
   * The Quiet section: no fill, no border, no shadow — a title row over flush
   * content, set off from what is above it by one hairline. (A framed `boxed`
   * variant was dropped once nothing used it: stack traces, JSON trees and
   * side rails all read fine as sections.)
   */
  interface Props {
    title?: string;
    class?: string;
    header?: Snippet;
    actions?: Snippet;
    /**
     * Rendered below the body, flush to the card's edges.
     *
     * Unlike `children` it is NOT wrapped in `card-body`, so it receives none
     * of the `padding` prop — a footer supplies its own inset. That is what
     * lets a pager sit at the same 18px as `card-head` regardless of how the
     * body is padded, instead of inheriting one padding and adding another.
     */
    footer?: Snippet;
    /**
     * Fold the body away behind a chevron in the head. The toggle wraps the
     * title (or the `header` snippet, which must then be phrasing content —
     * spans, not divs or headings — since it lands inside a `<button>`).
     * `actions` stay outside the toggle so their clicks don't fold the card.
     */
    collapsible?: boolean;
    /** Expanded state when `collapsible`. Bindable; starts open. */
    open?: boolean;
    children: Snippet;
  }

  let {
    title,
    class: klass = '',
    header,
    actions,
    footer,
    collapsible = false,
    open = $bindable(true),
    children,
  }: Props = $props();

  const bodyId = `card-body-${++nextId}`;
  const shown = $derived(!collapsible || open);
</script>

{#snippet toggle(label: Snippet)}
  <button
    type="button"
    class="card-toggle"
    aria-expanded={open}
    aria-controls={bodyId}
    onclick={() => (open = !open)}
  >
    <span class="chev"><Icon name="chevron-down" size={15} /></span>
    {@render label()}
  </button>
{/snippet}

{#snippet titleText()}{title}{/snippet}

<section class="card {klass}" class:collapsed={!shown}>
  {#if title || header || actions}
    <header class="card-head">
      <div class="head-left">
        {#if collapsible && header}
          {@render toggle(header)}
        {:else if collapsible && title}
          <h3 class="card-title">{@render toggle(titleText)}</h3>
        {:else if header}{@render header()}{:else if title}<h3 class="card-title">{title}</h3>{/if}
      </div>
      {#if actions}<div class="head-actions">{@render actions()}</div>{/if}
    </header>
  {/if}
  <div id={bodyId} class="card-body" hidden={!shown}>
    {@render children()}
  </div>
  {#if footer && shown}{@render footer()}{/if}
</section>

<style>
  /* A section reads as a section through its title and one hairline above
     it, not through a box. Boxes everywhere were the single biggest source of
     visual noise: a page of eight cards drew sixteen borders around content
     that needed none of them. */
  .card {
    min-width: 0;
    border-top: 1px solid var(--border);
    padding-top: 14px;
  }
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 28px;
    margin-bottom: 12px;
  }
  .card-title {
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
  }
  .head-left {
    min-width: 0;
  }
  /* A folded card is just its head: no gap under it. */
  .collapsed .card-head {
    margin-bottom: 0;
  }
  .card-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: start;
    cursor: pointer;
  }
  .chev {
    display: inline-flex;
    flex-shrink: 0;
    color: var(--text-faint);
    transition: transform 0.12s ease;
  }
  .collapsed .chev {
    transform: rotate(-90deg);
  }
  :global([dir='rtl']) .collapsed .chev {
    transform: rotate(90deg);
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
</style>
