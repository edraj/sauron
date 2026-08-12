<script lang="ts">
  import { fetchSchema, getAutocompleteSuggestions, type SchemaDefinition, type SearchContext } from '../../api/schema';
  import { parseQuery, type QueryNode } from '../../utils/query-parser';

  interface Props {
    appId: string;
    context?: SearchContext | string;
    value?: string;
    placeholder?: string;
    onSearch?: (query: string, ast?: QueryNode) => void;
    onChange?: (query: string) => void;
  }

  let {
    appId,
    context = 'issues',
    value = $bindable(''),
    placeholder = 'Search with query or variables (e.g. @tag=v1)',
    onSearch,
    onChange,
  }: Props = $props();

  let schema = $state<SchemaDefinition | null>(null);
  let suggestions = $state<string[]>([]);
  let showSuggestions = $state(false);
  let selectedIndex = $state(-1);
  let errorMsg = $state<string | null>(null);

  async function loadSchema() {
    if (!appId) return;
    try {
      errorMsg = null;
      schema = await fetchSchema(appId, context);
    } catch (err: any) {
      errorMsg = err?.message || 'Failed to load search schema';
    }
  }

  $effect(() => {
    if (appId && context) {
      loadSchema();
    }
  });

  function handleInput(e: Event) {
    const target = e.target as HTMLInputElement;
    value = target.value;
    onChange?.(value);

    if (schema) {
      const words = value.split(/\s+/);
      const currentWord = words[words.length - 1] || '';
      if (currentWord.startsWith('@') || currentWord.length > 0) {
        suggestions = getAutocompleteSuggestions(schema, currentWord);
        showSuggestions = suggestions.length > 0;
        selectedIndex = -1;
      } else {
        showSuggestions = false;
      }
    }
  }

  function selectSuggestion(suggestion: string) {
    const words = value.split(/\s+/);
    words[words.length - 1] = suggestion;
    value = words.join(' ') + ' ';
    showSuggestions = false;
    onChange?.(value);
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (showSuggestions && suggestions.length > 0) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        selectedIndex = (selectedIndex + 1) % suggestions.length;
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        selectedIndex = (selectedIndex - 1 + suggestions.length) % suggestions.length;
        return;
      }
      if ((e.key === 'Enter' || e.key === 'Tab') && selectedIndex >= 0) {
        e.preventDefault();
        selectSuggestion(suggestions[selectedIndex]);
        return;
      }
      if (e.key === 'Escape') {
        showSuggestions = false;
        return;
      }
    }

    if (e.key === 'Enter') {
      e.preventDefault();
      triggerSearch();
    }
  }

  function triggerSearch() {
    let ast: QueryNode | undefined = undefined;
    try {
      ast = parseQuery(value);
    } catch {
      /* ignore invalid query parsing on submit */
    }
    onSearch?.(value, ast);
  }
</script>

<div class="search-autocomplete-container relative w-full">
  <div class="flex items-center gap-2">
    <input
      type="text"
      role="combobox"
      aria-expanded={showSuggestions}
      aria-autocomplete="list"
      aria-controls="autocomplete-listbox"
      class="w-full px-3 py-2 border rounded-md shadow-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
      {placeholder}
      bind:value
      oninput={handleInput}
      onkeydown={handleKeyDown}
    />
    <button
      type="button"
      class="px-4 py-2 bg-blue-600 text-white font-medium rounded-md hover:bg-blue-700"
      onclick={triggerSearch}
    >
      Search
    </button>
  </div>

  {#if errorMsg}
    <div class="text-xs text-red-500 mt-1">{errorMsg}</div>
  {/if}

  {#if showSuggestions && suggestions.length > 0}
    <ul
      id="autocomplete-listbox"
      role="listbox"
      class="absolute z-10 w-full mt-1 bg-white border rounded-md shadow-lg max-h-60 overflow-auto"
    >
      {#each suggestions as item, idx}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <li
          role="option"
          aria-selected={idx === selectedIndex}
          class="px-3 py-2 cursor-pointer hover:bg-blue-50 {idx === selectedIndex ? 'bg-blue-100 font-semibold' : ''}"
          onclick={() => selectSuggestion(item)}
        >
          {item}
        </li>
      {/each}
    </ul>
  {/if}
</div>
