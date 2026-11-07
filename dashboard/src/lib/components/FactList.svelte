<!--
  The one key/value layout for curated facts on detail pages (OS, first
  seen, release…). Each value is a snippet, so it can hold a TimeValue, a
  badge or a link. Four detail pages each hand-rolled their own <dl> with
  different label colours, gaps and alignment; `KeyValueList` stays the
  renderer for raw tag/context maps.

  Usage:
    <FactList>
      <Fact label="OS">Android 14</Fact>
      <Fact label="First seen"><TimeValue value={x} /></Fact>
      <Fact label="Device id" mono>{id}</Fact>
    </FactList>
-->
<script lang="ts">
  import type { Snippet } from 'svelte';

  let { children }: { children: Snippet } = $props();
</script>

<dl class="facts">{@render children()}</dl>

<style>
  .facts {
    display: grid;
    grid-template-columns: minmax(96px, max-content) minmax(0, 1fr);
    column-gap: 16px;
    row-gap: 9px;
    margin: 0;
    font-size: var(--text-sm);
  }
</style>
