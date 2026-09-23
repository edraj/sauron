<!--
  The row of controls that narrows or reloads the list beneath it.

  One layout, used by every list page, so the search box is in the same place
  with the same neighbours everywhere: `[+ Add filter] [search — grows]
  [window] [status · refresh · export]`. Before this each page composed its
  own `.controls` row — search in the page header on three pages, in a section
  header on two, in a standalone bar on four; the window before the search on
  some and after it on others; a fixed 240/260/280/300px box depending on the
  page. Same controls, six arrangements.

  Two rules this component exists to enforce:

  - **Top-aligned, never centred.** `align-items: center` is what made the row
    jump: the query box renders its error line inside its own root, so the
    moment it appeared every sibling re-centred against a taller box. Every
    control is `--control-h` tall, so top-aligning gives the same visual
    alignment as centring and a message under the search box moves nothing
    else.
  - **The first line never changes shape.** Filter chips and the chip editor
    render on a second line that exists only while there is something to show,
    so adding a chip cannot push the search box sideways or shrink it.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    /** Leading control — the filter builder's "+ Add filter". */
    lead?: Snippet;
    /**
     * The search box. Takes whatever width the other slots leave.
     *
     * Named `searchBox`, not `search`, because a page declaring the snippet
     * inline (`{#snippet search()}`) would shadow its own `search` state —
     * the very value the box binds to. Same reason `timeWindow` is not `range`.
     */
    searchBox?: Snippet;
    /** The window control: `DateRange` pills or a `TimeFilter`. */
    timeWindow?: Snippet;
    /** Status and page actions: freshness, refresh, export. */
    actions?: Snippet;
    /** The second line: filter chips, the draft chip. Only rendered when set. */
    below?: Snippet;
  }

  let { lead, searchBox, timeWindow, actions, below }: Props = $props();
</script>

<div class="list-toolbar">
  <div class="row">
    {#if lead}<div class="slot lead">{@render lead()}</div>{/if}
    {#if searchBox}<div class="slot search">{@render searchBox()}</div>{/if}
    {#if timeWindow}<div class="slot range">{@render timeWindow()}</div>{/if}
    {#if actions}<div class="slot actions">{@render actions()}</div>{/if}
  </div>
  {#if below}
    <div class="below">{@render below()}</div>
  {/if}
</div>

<style>
  .list-toolbar {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 16px;
  }
  .row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    flex-wrap: wrap;
  }
  .slot {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: var(--control-h);
  }
  /* Block, not flex: the search component's own root fills it, and the error
     line it may render below its shell stacks naturally. */
  .slot.search {
    display: block;
    flex: 1 1 280px;
    min-width: 240px;
  }
  .slot.search > :global(*) {
    width: 100%;
  }
  /* Keeps the actions at the end of the line even when a page has no search
     slot, or when the row wraps and the actions land on a line of their own. */
  .slot.actions {
    margin-inline-start: auto;
    flex-wrap: wrap;
  }
  .below {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
</style>
