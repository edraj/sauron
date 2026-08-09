<script lang="ts">
  import { relativeTime, formatTimestamp } from '../utils/format';
  import { timeFormatStore } from '../stores/time-format.svelte';

  interface Props {
    value: string | number | Date | null | undefined;
    /** Apply the muted text colour, as most table cells here do. */
    muted?: boolean;
    /**
     * Render as plain text with no toggle. For the handful of places that need
     * a formatted instant inside another control's label or a StatTile `sub`
     * slot, where a nested button would be invalid markup.
     */
    asText?: boolean;
  }

  let { value, muted = false, asText = false }: Props = $props();

  const isRelative = $derived(timeFormatStore.mode === 'relative');
  const shown = $derived(isRelative ? relativeTime(value) : formatTimestamp(value));
  // The other representation stays in the tooltip, so hovering still answers
  // the question without a click.
  const other = $derived(isRelative ? formatTimestamp(value) : relativeTime(value));
  const empty = $derived(shown === '—');
</script>

{#if empty || asText}
  <span class="tv" class:muted>{shown}</span>
{:else}
  <button
    type="button"
    class="tv"
    class:muted
    title={other}
    aria-label={`${shown} — click to show ${isRelative ? 'exact time' : 'relative time'}`}
    onclick={(e) => {
      // Many of these sit in `tr.clickable` rows that navigate or expand on
      // click. Stopping here — on the button itself — bounds the guard to the
      // toggle's own hit area, so the rest of the cell (and the plain `<span>`
      // rendered for a null value) stays part of the clickable row.
      e.stopPropagation();
      timeFormatStore.toggle();
    }}
  >
    {shown}
  </button>
{/if}

<style>
  /* Styled to read as text, not a control: a table of fifty things that all
     look like buttons is worse than the tooltips this replaces. The affordance
     is the dotted underline on hover. */
  .tv {
    font: inherit;
    color: inherit;
  }
  .tv.muted {
    color: var(--text-muted);
  }
  button.tv {
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    cursor: pointer;
    text-align: inherit;
    white-space: nowrap;
  }
  button.tv:hover {
    text-decoration: underline dotted;
  }
  button.tv:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 2px;
  }
</style>
