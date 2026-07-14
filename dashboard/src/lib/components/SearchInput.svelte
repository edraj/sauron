<script lang="ts">
  import Icon from './ui/Icon.svelte';

  interface Props {
    value: string;
    placeholder?: string;
    oninput?: (value: string) => void;
    width?: string;
  }

  let {
    value = $bindable(''),
    placeholder = 'Search…',
    oninput,
    width = '260px',
  }: Props = $props();

  function handle(e: Event) {
    const v = (e.target as HTMLInputElement).value;
    value = v;
    oninput?.(v);
  }

  function clear() {
    value = '';
    oninput?.('');
  }
</script>

<div class="search" style="--w:{width}">
  <span class="ic" aria-hidden="true"><Icon name="search" size={15} /></span>
  <input
    type="search"
    {placeholder}
    {value}
    oninput={handle}
    spellcheck="false"
    autocomplete="off"
  />
  {#if value}
    <button class="clear" onclick={clear} type="button" aria-label="Clear search"><Icon name="x" size={14} /></button>
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
</style>
