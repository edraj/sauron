<script lang="ts">
  import type { Snippet } from 'svelte';

  // A dense, Linear-style table shell. The parent supplies the <tr>/<th>/<td>
  // markup via the `head` and `children` snippets, so each screen keeps full
  // control of its columns while inheriting consistent styling, a sticky header,
  // hover rows, and horizontal overflow scrolling.
  //
  // Add class="clickable" to a <tr> to get the pointer + hover-lift affordance.
  interface Props {
    head: Snippet;
    children: Snippet;
    class?: string;
  }

  let { head, children, class: klass = '' }: Props = $props();
</script>

<div class="dt-wrap {klass}">
  <table class="dt">
    <thead>
      {@render head()}
    </thead>
    <tbody>
      {@render children()}
    </tbody>
  </table>
</div>

<style>
  /* No frame: a table is rows and hairlines, aligned with the page text. The
     wrapper only scrolls — a wide table scrolls inside itself, never the page. */
  .dt-wrap {
    width: 100%;
    overflow-x: auto;
  }
  .dt {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  .dt :global(thead th) {
    position: sticky;
    top: 0;
    z-index: 1;
    text-align: start;
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    color: var(--text-faint);
    background: var(--dt-bg);
    padding: 8px 12px;
    white-space: nowrap;
    border-bottom: 1px solid var(--border);
  }
  .dt :global(th.num),
  .dt :global(td.num) {
    text-align: end;
    font-variant-numeric: tabular-nums;
  }
  .dt :global(tbody td) {
    padding: 11px 12px;
    border-bottom: 1px solid var(--border);
    color: var(--text);
    vertical-align: middle;
    white-space: nowrap;
  }
  /* The outer columns sit on the page's text edge, as the header and the
     sections around the table do. */
  .dt :global(tr > :first-child) {
    padding-inline-start: 2px;
  }
  .dt :global(tr > :last-child) {
    padding-inline-end: 2px;
  }
  .dt :global(tbody tr:last-child td) {
    border-bottom: none;
  }
  .dt :global(tbody tr.clickable) {
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .dt :global(tbody tr.clickable:hover) {
    background: var(--dt-hover);
  }
  /* The first cell of a navigable row is a real `<a href>`, so the row can be
     opened in a new tab with the middle button or the context menu. It has to
     read as the cell it replaced until hovered, or every list grows a column of
     blue: the colour is inherited and only the underline marks it as a link. */
  .dt :global(td .row-link) {
    color: inherit;
    text-decoration: none;
  }
  /* Keyed off a hover on the ROW, not the anchor, so the underline tracks the
     real click target — the whole row navigates, not just the link. */
  .dt :global(tbody tr.clickable:hover td .row-link) {
    text-decoration: underline;
  }
  .dt :global(td .cell-mono) {
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
  }
  .dt :global(td .cell-muted) {
    color: var(--text-muted);
  }
  .dt :global(td.wrap) {
    white-space: normal;
  }
</style>
