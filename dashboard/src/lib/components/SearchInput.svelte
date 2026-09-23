<!--
  The plain search box — a term the server matches, no query language.

  Shares its shell with `SearchAutocompleteInput`: the same `--control-h`
  height, surface, border, radius, focus ring, icon, clear button and Search
  button. The two used to differ by a few pixels in every one of those, which
  is how the Users and Sessions toolbars — one page apart, same controls —
  read as two different products.

  Width is the parent's to decide. It used to be a `width` prop that every
  page set differently (240, 260, 280, 300px); inside a `ListToolbar` the box
  fills its slot, and the prop remains only for the in-card uses that need a
  fixed box.
-->
<script lang="ts">
  import { t } from '../i18n';
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
    /** A fixed width. Leave unset inside a toolbar, where the box fills its slot. */
    width?: string;
  }

  let {
    value = $bindable(''),
    placeholder = 'Search…',
    oninput,
    onsearch,
    width = undefined,
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

<div class="search" class:has-go={!!onsearch} style:width={width ?? '100%'}>
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
    <button class="clear" onclick={clear} type="button" aria-label={t('ui.search.clear')}><Icon name="x" size={14} /></button>
  {/if}
  {#if onsearch}
    <!--
      Never `disabled` when the text is unchanged: re-running the same query is
      a legitimate thing to want. The pending colour says "the rows below are
      not showing what you typed"; the tooltip spells it out.
    -->
    <button
      class="go"
      class:pending
      type="button"
      onclick={submit}
      title={pending ? t('ui.search.pending') : t('ui.search.submit')}
    >
      {t('common.search')}
    </button>
  {/if}
</div>

<style>
  .search {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: var(--control-h);
    max-width: 100%;
    padding-inline: 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 0.13s ease, box-shadow 0.13s ease;
  }
  .search.has-go {
    padding-inline-end: 3px;
  }
  .search:focus-within {
    border-color: var(--primary-border);
    box-shadow: 0 0 0 3px var(--primary-soft);
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
    height: 100%;
    padding: 0;
    background: none;
    border: none;
    color: var(--text);
    font-size: 13px;
    outline: none;
  }
  input::placeholder {
    color: var(--text-faint);
  }
  /* The browser's own clear affordance on `type="search"` would double ours. */
  input::-webkit-search-cancel-button,
  input::-webkit-search-decoration {
    -webkit-appearance: none;
    appearance: none;
  }
  .clear {
    display: inline-flex;
    align-items: center;
    background: none;
    border: none;
    color: var(--text-faint);
    padding: 2px;
    border-radius: var(--radius-sm);
  }
  .clear:hover {
    color: var(--text);
  }
  .go {
    flex-shrink: 0;
    height: 28px;
    padding: 0 11px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    transition: color 0.13s ease, border-color 0.13s ease, background 0.13s ease;
  }
  .go:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .go.pending {
    background: var(--primary);
    border-color: transparent;
    color: var(--primary-contrast);
  }
  .go.pending:hover {
    background: var(--primary-hover);
  }
</style>
