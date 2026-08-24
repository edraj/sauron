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
  .dt-wrap {
    width: 100%;
    overflow-x: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
  }
  .dt {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  .dt :global(thead th) {
    position: sticky;
    top: 0;
    z-index: 1;
    text-align: start;
    font-size: 11px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    background: var(--surface-2);
    padding: 9px 14px;
    white-space: nowrap;
    border-bottom: 1px solid var(--border);
  }
  .dt :global(th.num),
  .dt :global(td.num) {
    text-align: end;
    font-variant-numeric: tabular-nums;
  }
  .dt :global(tbody td) {
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
    color: var(--text);
    vertical-align: middle;
    white-space: nowrap;
  }
  .dt :global(tbody tr:last-child td) {
    border-bottom: none;
  }
  .dt :global(tbody tr.clickable) {
    cursor: pointer;
    transition: background 0.1s ease;
  }
  .dt :global(tbody tr.clickable:hover) {
    background: var(--surface-2);
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
     real click target — the whole row navigates, not just the link.
     `tr.clickable` lives in this component's own markup, so no `:global` gymnastics
     are needed here the way they are in the pages that style their own cells. */
  .dt :global(tbody tr.clickable:hover td .row-link) {
    text-decoration: underline;
  }
  .dt :global(td .cell-mono) {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .dt :global(td .cell-muted) {
    color: var(--text-muted);
  }
  .dt :global(td.wrap) {
    white-space: normal;
  }
</style>
