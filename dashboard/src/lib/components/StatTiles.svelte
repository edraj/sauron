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
  .stat-tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(var(--tile-min), 1fr));
    gap: 12px;
  }
</style>
