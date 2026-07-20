<!--
  Icon-only refresh control for page headers. Re-fetches the current view via the
  supplied `onclick`; while `loading` is true the icon spins and the button is
  disabled (prevents double-firing). Sized and styled to sit beside DateRange /
  SearchInput in a page's `.controls` row (matches the Topbar `.icon-btn`).
-->
<script lang="ts">
  import Icon from './Icon.svelte';

  interface Props {
    /** Called on click; should re-run the page's data load. */
    onclick?: () => void;
    /** Spins the icon and disables the button while a refresh is in flight. */
    loading?: boolean;
    /** Tooltip / accessible label. */
    title?: string;
  }

  let { onclick, loading = false, title = 'Refresh' }: Props = $props();
</script>

<button
  class="refresh-btn"
  type="button"
  {title}
  aria-label={title}
  disabled={loading}
  onclick={() => onclick?.()}
>
  <span class="ic" class:spinning={loading}><Icon name="refresh" size={16} /></span>
</button>

<style>
  .refresh-btn {
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-muted);
    transition: color 0.13s ease, background 0.13s ease, border-color 0.13s ease;
    flex-shrink: 0;
  }
  .refresh-btn:hover:not(:disabled) {
    color: var(--text);
    background: var(--surface-3);
    border-color: var(--border-strong);
  }
  .refresh-btn:disabled {
    cursor: default;
  }
  .ic {
    display: inline-flex;
    color: inherit;
  }
  .ic.spinning {
    animation: refresh-spin 0.7s linear infinite;
  }
  @keyframes refresh-spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .ic.spinning {
      animation-duration: 1.4s;
    }
  }
</style>
