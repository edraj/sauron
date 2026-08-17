<script lang="ts">
  import Icon from './ui/Icon.svelte';

  interface Props {
    value: string;
    placeholder?: string;
    /**
     * Fired on every keystroke. Correct for a box that filters rows already in
     * the browser; wrong for one that queries the server — use `onsearch`.
     */
    oninput?: (value: string) => void;
    /**
     * Fired only on an explicit submit (button, Enter, clear). Passing this
     * turns the box into a submit-driven search: a Search button appears and
     * typing stops being a trigger for anything.
     */
    onsearch?: (value: string) => void;
    width?: string;
  }

  let {
    value = $bindable(''),
    placeholder = 'Search…',
    oninput,
    onsearch,
    width = '260px',
  }: Props = $props();

  /** Seeded from the initial value so a URL-hydrated box starts settled. */
  let lastSubmitted = $state(value);
  const pending = $derived(!!onsearch && value.trim() !== lastSubmitted.trim());

  function handle(e: Event) {
    const v = (e.target as HTMLInputElement).value;
    value = v;
    oninput?.(v);
  }

  function submit() {
    lastSubmitted = value;
    onsearch?.(value);
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (!onsearch || e.key !== 'Enter') return;
    e.preventDefault();
    submit();
  }

  function clear() {
    value = '';
    lastSubmitted = '';
    oninput?.('');
    // Clearing applies at once — leaving the old term filtering an empty box
    // would describe the rows below wrongly.
    onsearch?.('');
  }
</script>

<div class="search" style="--w:{width}">
  <span class="ic" aria-hidden="true"><Icon name="search" size={15} /></span>
  <input
    type="search"
    {placeholder}
    {value}
    oninput={handle}
    onkeydown={handleKeyDown}
    spellcheck="false"
    autocomplete="off"
  />
  {#if value}
    <button class="clear" onclick={clear} type="button" aria-label="Clear search"><Icon name="x" size={14} /></button>
  {/if}
  {#if onsearch}
    <button class="go" class:pending type="button" onclick={submit} title="Search (Enter)">Search</button>
  {/if}
</div>

<style>
  .search {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    width: var(--w);
    max-width: 100%;
    padding: 0 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 0.13s ease;
  }
  .search:focus-within {
    border-color: var(--primary-border);
  }
  .ic {
    display: inline-flex;
    align-items: center;
    color: var(--text-faint);
    flex-shrink: 0;
  }
  input {
    flex: 1;
    min-width: 0;
    padding: 8px 0;
    background: none;
    border: none;
    color: var(--text);
    outline: none;
  }
  input::placeholder {
    color: var(--text-faint);
  }
  .clear {
    display: inline-flex;
    align-items: center;
    background: none;
    border: none;
    color: var(--text-faint);
    padding: 2px;
  }
  .clear:hover {
    color: var(--text);
  }
  .go {
    flex-shrink: 0;
    margin: 3px -7px 3px 2px;
    padding: 5px 11px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 540;
  }
  .go:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .go.pending {
    background: var(--primary-soft);
    border-color: var(--primary-border);
    color: var(--primary);
  }
</style>
