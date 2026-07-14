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
    text-align: left;
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
    text-align: right;
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
