<!--
  The query-language search box.

  Styled with the dashboard's own CSS custom properties, like every other
  control here. It previously carried Tailwind utility classes (`bg-white`,
  `bg-blue-600`, `rounded-md`, `shadow-lg`) — and this project has no Tailwind,
  so none of them resolved: the input ignored every design token and the
  suggestion list rendered with NO background at all, drawing transparent over
  the table beneath it. `SearchInput.svelte` is the house reference this now
  follows.

  There is no submit button. The query is already live via the debounced
  `onChange` every page drives, so a button would only restate what typing
  already did — and the old one called an `onSearch` prop no call site ever
  passed.
-->
<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import {
    fetchSchema,
    getAutocompleteSuggestions,
    placeholderFor,
    type SchemaDefinition,
    type Suggestion,
  } from '../../api/schema';

  interface Props {
    appId: string;
    context?: string;
    value?: string;
    /** Overrides the schema-derived default. Prefer letting it generate. */
    placeholder?: string;
    /** A query error to mark inline — fed by the page from its 400/403. */
    error?: string | null;
    onChange?: (query: string) => void;
  }

  let {
    appId,
    context = 'issues',
    value = $bindable(''),
    placeholder = undefined,
    error = null,
    onChange,
  }: Props = $props();

  let schema = $state<SchemaDefinition | null>(null);
  let schemaError = $state<string | null>(null);
  let suggestions = $state<Suggestion[]>([]);
  let open = $state(false);
  let selectedIndex = $state(-1);
  let inputEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLUListElement | null>(null);
  let rootEl = $state<HTMLDivElement | null>(null);

  const effectivePlaceholder = $derived(placeholder ?? placeholderFor(schema));

  $effect(() => {
    // Track both, so a context switch refetches.
    const id = appId;
    const ctx = context;
    if (!id || !ctx) {
      schema = null;
      return;
    }
    let cancelled = false;
    fetchSchema(id, ctx)
      .then((s) => {
        if (!cancelled) {
          schema = s;
          schemaError = null;
        }
      })
      .catch((err: unknown) => {
        // A degraded autocomplete must never block typing a query: the input
        // stays fully usable and only the suggestions go away.
        if (!cancelled) {
          schema = null;
          schemaError = err instanceof Error ? err.message : 'Suggestions unavailable';
        }
      });
    return () => {
      cancelled = true;
    };
  });

  /** The token the caret sits in — suggestions complete this, not the line. */
  function currentToken(v: string): string {
    const parts = v.split(/\s+/);
    return parts[parts.length - 1] ?? '';
  }

  function refresh() {
    if (!schema) {
      open = false;
      return;
    }
    suggestions = getAutocompleteSuggestions(schema, currentToken(value));
    open = suggestions.length > 0;
    selectedIndex = -1;
  }

  function handleInput(e: Event) {
    value = (e.target as HTMLInputElement).value;
    onChange?.(value);
    refresh();
  }

  function apply(s: Suggestion) {
    const parts = value.split(/\s+/);
    parts[parts.length - 1] = s.insert;
    // A field completion ends in `:` and must NOT gain a trailing space — the
    // caret stays inside the token so the value suggestions open immediately.
    value = parts.join(' ') + (s.insert.endsWith(':') ? '' : ' ');
    onChange?.(value);
    inputEl?.focus();
    refresh();
  }

  function move(delta: number) {
    if (!suggestions.length) return;
    selectedIndex = (selectedIndex + delta + suggestions.length) % suggestions.length;
    // Arrowing past the visible window must follow the highlight.
    queueMicrotask(() => {
      listEl?.querySelector<HTMLElement>(`#sac-opt-${selectedIndex}`)?.scrollIntoView({
        block: 'nearest',
      });
    });
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (open && suggestions.length) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        move(1);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        move(-1);
        return;
      }
      if ((e.key === 'Enter' || e.key === 'Tab') && selectedIndex >= 0) {
        e.preventDefault();
        apply(suggestions[selectedIndex]);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        open = false;
        return;
      }
    }
    if (e.key === 'Enter') {
      // The query is already live via the debounced `onChange`; Enter just
      // dismisses, so the reader is not left with a list covering their rows.
      e.preventDefault();
      open = false;
    }
  }

  function clear() {
    value = '';
    onChange?.('');
    open = false;
    inputEl?.focus();
  }
</script>

<!--
  `pointerdown`, NOT `click`.

  On `click` this closed the menu every time a suggestion was picked. The
  handler runs after `apply()` has already replaced the suggestion list, so the
  button that was clicked is detached from the document by then and
  `rootEl.contains(target)` answers false — indistinguishable from a click on
  the page behind. `pointerdown` fires before any of that, while the target is
  still in the tree.

  Only the live drive caught this: the value list simply never appeared, and
  every unit test and type check passed.
-->
<svelte:window
  onpointerdown={(e) => {
    if (!(e.target instanceof Node)) return;
    if (rootEl && !rootEl.contains(e.target)) open = false;
  }}
/>

<div class="sac" bind:this={rootEl}>
  <div class="shell" class:invalid={!!error}>
    <span class="ic" aria-hidden="true"><Icon name="search" size={15} /></span>
    <input
      bind:this={inputEl}
      type="text"
      role="combobox"
      aria-expanded={open}
      aria-autocomplete="list"
      aria-controls="sac-listbox"
      aria-activedescendant={selectedIndex >= 0 ? `sac-opt-${selectedIndex}` : undefined}
      aria-invalid={!!error}
      spellcheck="false"
      autocomplete="off"
      placeholder={effectivePlaceholder}
      {value}
      oninput={handleInput}
      onkeydown={handleKeyDown}
      onfocus={refresh}
      onblur={() => setTimeout(() => (open = false), 120)}
    />
    {#if value}
      <button class="clear" type="button" aria-label="Clear search" onclick={clear}>
        <Icon name="x" size={14} />
      </button>
    {/if}
  </div>

  {#if error}
    <p class="msg err" role="alert">{error}</p>
  {:else if schemaError}
    <p class="msg hint">{schemaError} — you can still type a query.</p>
  {/if}

  {#if open && suggestions.length}
    <ul bind:this={listEl} id="sac-listbox" role="listbox" class="menu">
      {#each suggestions as s, idx (s.insert)}
        <li
          id="sac-opt-{idx}"
          role="option"
          aria-selected={idx === selectedIndex}
          class:sel={idx === selectedIndex}
        >
          <button type="button" onmousedown={(e) => e.preventDefault()} onclick={() => apply(s)}>
            <span class="lbl">{s.label}</span>
            {#if s.detail}<span class="det">{s.detail}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .sac {
    position: relative;
    flex: 1;
    min-width: 260px;
  }
  .shell {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 0.13s ease;
  }
  .shell:focus-within {
    border-color: var(--primary-border);
  }
  .shell.invalid {
    border-color: var(--error);
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
    font-family: var(--font-mono);
    font-size: 12.5px;
  }
  input::placeholder {
    color: var(--text-faint);
    font-family: var(--font-sans);
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
  .msg {
    margin: 4px 2px 0;
    font-size: 11.5px;
  }
  .msg.err {
    color: var(--error);
  }
  .msg.hint {
    color: var(--text-faint);
  }
  .menu {
    position: absolute;
    z-index: 30;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    margin: 0;
    padding: 4px;
    list-style: none;
    max-height: 260px;
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lg);
  }
  .menu li button {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    padding: 6px 8px;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--text);
    text-align: left;
    font-size: 12.5px;
  }
  .menu li.sel button,
  .menu li button:hover {
    background: var(--primary-soft);
    color: var(--primary);
  }
  .lbl {
    font-family: var(--font-mono);
  }
  .det {
    color: var(--text-faint);
    font-size: 11px;
    flex-shrink: 0;
  }
</style>
