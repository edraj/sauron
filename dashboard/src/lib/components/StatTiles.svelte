<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    // Minimum tile width before wrapping; controls the responsive column count.
    min?: number;
    children: Snippet;
  }

  let { min = 150, children }: Props = $props();
</script>

<div class="stat-tiles" style="--tile-min:{min}px">
  {@render children()}
</div>

<style>
  /* One strip of figures divided by hairlines, not a row of boxes. The lines
     are each tile's own shadow drawn into the 1px gap (left and top), and the
     container clips the ones that would fall outside it — so a wrapped last
     row stays divided without the empty grid area beside it filling in. */
  .stat-tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(var(--tile-min), 1fr));
    gap: 1px;
    overflow: hidden;
    /* Pull the strip out by one tile inset so the first column's figures sit on
       the page's text edge; the clipped overflow hides that column's divider. */
    margin-inline-start: -20px;
  }
</style>
