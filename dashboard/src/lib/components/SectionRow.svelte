<!--
  One row inside a `CollapsibleFetchCard`, carrying TWO independent actions.

  The distinction matters and is easy to collapse by accident: pressing the row
  expands it in place to show the full record, while the trailing arrow leaves
  the page for that record's own detail view. Making the whole row a link would
  cost the inline view; making it expand-only would strip the navigation the
  cards exist to offer. So the summary is a button, the arrow is a separate
  control beside it, and neither is nested inside the other — a button inside a
  button is invalid HTML and the browser's recovery drops one of them.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './ui/Icon.svelte';

  interface Props {
    /** The always-visible summary line. */
    children: Snippet;
    /** Revealed when the row is expanded. */
    expanded: Snippet;
    /**
     * Navigate to this record's own page. `null` means there is none — an
     * analytics event has no detail route — and the arrow is then omitted
     * rather than rendered inert.
     */
    onopen?: (() => void) | null;
    /** Accessible name for the arrow, e.g. "Open device". */
    openLabel?: string;
  }

  let { children, expanded, onopen = null, openLabel = 'Open' }: Props = $props();

  let isOpen = $state(false);
</script>

<div class="row">
  <button
    class="summary"
    onclick={() => (isOpen = !isOpen)}
    aria-expanded={isOpen}
  >
    <Icon name={isOpen ? 'chevron-down' : 'chevron-right'} size={12} />
    {@render children()}
  </button>
  {#if onopen}
    <button class="open" onclick={onopen} title={openLabel} aria-label={openLabel}>
      <Icon name="arrow-up-right" size={13} />
    </button>
  {/if}
</div>

{#if isOpen}
  <div class="detail">{@render expanded()}</div>
{/if}

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 0;
  }
  .summary {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    padding: 0;
    color: var(--text);
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
  }
  .summary:hover {
    color: var(--primary);
  }
  .open {
    flex: none;
    display: grid;
    place-items: center;
    background: none;
    border: none;
    padding: 4px;
    border-radius: var(--radius-sm, 4px);
    color: var(--text-faint);
    cursor: pointer;
  }
  .open:hover {
    color: var(--primary);
    background: var(--surface-alt, transparent);
  }
  .detail {
    padding: 4px 0 12px 20px;
  }
</style>
