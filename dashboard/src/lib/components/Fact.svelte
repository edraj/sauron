<!-- One row of a `FactList`. `mono` for identifiers (ids, hashes, versions). -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    label: string;
    mono?: boolean;
    children: Snippet;
  }

  let { label, mono = false, children }: Props = $props();
</script>

<dt>{label}</dt>
<dd class:mono>{@render children()}</dd>

<style>
  dt {
    color: var(--text-muted);
    font-size: var(--text-xs);
    line-height: 20px;
    white-space: nowrap;
  }
  dd {
    margin: 0;
    min-width: 0;
    color: var(--text);
    line-height: 20px;
    overflow-wrap: anywhere;
  }
  dd.mono {
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
  }
</style>
