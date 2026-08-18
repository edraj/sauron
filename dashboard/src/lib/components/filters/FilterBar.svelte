<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import SearchAutocompleteInput from '../search/SearchAutocompleteInput.svelte';
  import DateRange from '../DateRange.svelte';
  import {
    OP_LABEL,
    composeTag,
    isFilterValueValid,
    normalizeFilterValue,
    type FieldDef,
    type Filter,
    type Op,
  } from './filters';

  interface Props {
    fields: FieldDef[];
    filters: Filter[];
    search: string;
    appId?: string;
    context?: string;
    /** A query error from the page's last request, marked on the input. */
    error?: string | null;
    /**
     * Run the search box's current text. The box never fires on input, so a
     * page that binds `search` but omits this gets a control that types and
     * validates and never queries.
     */
    onSearch?: (query: string) => void;
    sinceDays: number;
    // Optional custom date-range options; falls back to DateRange's default.
    ranges?: { days: number; label: string }[];
    /**
     * Render the built-in date range.
     *
     * `false` for a page that owns its window through a richer control (a
     * `<TimeFilter>`, which also picks the timestamp COLUMN and accepts
     * absolute bounds). Those pages still pass `sinceDays` — the prop is
     * `$bindable` — but showing this control beside the real one puts two
     * range pickers on screen where only one is connected to anything, and a
     * dead control is worse than a missing one: it reports a window the list
     * is not using.
     */
    showRange?: boolean;
    /**
     * The page's own list controls — a `<TimeFilter>`, refresh, an export
     * button — rendered on the same line as the search box.
     *
     * They belong here rather than in the page header because every one of
     * them narrows or reloads the SAME table this bar's chips and search
     * narrow. Split across two places (the old arrangement) the search box sat
     * two sections above the rows it filtered, next to charts that ignore it
     * entirely, and read as if it filtered those instead.
     */
    actions?: Snippet;
    /**
     * A chip was added or removed.
     *
     * `bind:filters` already hands the page the new list; this exists for what
     * the binding CANNOT express — that the change happened, and so the page
     * position is now meaningless. Row 51 of the unfiltered set is not row 51
     * of the filtered one. Pages that page by cursor rebuild their position
     * from the predicate anyway and can ignore this.
     */
    onchange?: (filters: Filter[]) => void;
  }
  let {
    fields,
    filters = $bindable([]),
    search = $bindable(''),
    appId = undefined,
    context = undefined,
    error = null,
    onSearch = undefined,
    sinceDays = $bindable(30),
    ranges = undefined,
    showRange = true,
    actions = undefined,
    onchange = undefined,
  }: Props = $props();

  let adding = $state(false);
  let draftField = $state<string>('');
  let draftOp = $state<Op>('eq');
  let draftValue = $state('');
  let draftTagKey = $state('');
  let draftTagVal = $state('');

  const fieldDef = $derived(fields.find((f) => f.key === draftField));

  function openAdd() {
    adding = true;
    draftField = fields[0]?.key ?? '';
    draftOp = fields[0]?.ops[0] ?? 'eq';
    draftValue = fields[0]?.type === 'enum' ? (fields[0]?.options?.[0] ?? '') : '';
    draftTagKey = '';
    draftTagVal = '';
  }
  function onFieldChange() {
    const def = fields.find((f) => f.key === draftField);
    draftOp = def?.ops[0] ?? 'eq';
    draftValue = def?.type === 'enum' ? (def?.options?.[0] ?? '') : '';
    draftTagKey = '';
    draftTagVal = '';
  }
  function commit() {
    if (fieldDef?.type === 'tag') {
      if (!draftTagKey.trim() || !draftTagVal.trim()) return;
      filters = [...filters, { field: draftField, op: draftOp, value: composeTag(draftTagKey.trim(), draftTagVal.trim()) }];
      adding = false;
      onchange?.(filters);
      return;
    }
    const value = normalizeFilterValue(fieldDef, draftValue);
    if (!isFilterValueValid(fieldDef, value)) return;
    filters = [...filters, { field: draftField, op: draftOp, value }];
    adding = false;
    onchange?.(filters);
  }
  function remove(i: number) {
    filters = filters.filter((_, idx) => idx !== i);
    onchange?.(filters);
  }
  function labelFor(key: string): string {
    return fields.find((f) => f.key === key)?.label ?? key;
  }
</script>

<div class="filterbar">
  <div class="chips">
    {#each filters as f, i (i)}
      <span class="chip">
        <span class="c-field">{labelFor(f.field)}</span>
        <span class="c-op">{OP_LABEL[f.op]}</span>
        <span class="c-val mono">{f.value}</span>
        <button type="button" class="c-x" aria-label="Remove filter" onclick={() => remove(i)}>
          <Icon name="x" size={12} />
        </button>
      </span>
    {/each}

    {#if adding}
      <span class="draft">
        <select bind:value={draftField} onchange={onFieldChange} aria-label="Filter field">
          {#each fields as f (f.key)}<option value={f.key}>{f.label}</option>{/each}
        </select>
        <select bind:value={draftOp} aria-label="Operator">
          {#each fieldDef?.ops ?? [] as op (op)}<option value={op}>{OP_LABEL[op]}</option>{/each}
        </select>
        {#if fieldDef?.type === 'tag'}
          <input type="text" bind:value={draftTagKey} placeholder="key" aria-label="Tag key" class="tag-key" />
          <span class="tag-eq">=</span>
          <input type="text" bind:value={draftTagVal} placeholder="value" aria-label="Tag value" class="tag-val" />
        {:else if fieldDef?.type === 'enum'}
          <select bind:value={draftValue} aria-label="Value">
            {#each fieldDef?.options ?? [] as opt (opt)}<option value={opt}>{opt}</option>{/each}
          </select>
        {:else if fieldDef?.type === 'number'}
          <!-- Text, not type="number". `bind:value` on a numberlike input
               writes back a number (or null once cleared) rather than the
               string `Filter.value` is declared as, which is what let a
               cleared field commit `times_seen:eq:null`. -->
          <input type="text" inputmode="numeric" bind:value={draftValue} placeholder="value" aria-label="Value" />
        {:else}
          <input type="text" bind:value={draftValue} placeholder="value" aria-label="Value" />
        {/if}
        <button type="button" class="d-ok" onclick={commit}>Add</button>
        <button type="button" class="d-x" aria-label="Cancel" onclick={() => (adding = false)}>
          <Icon name="x" size={13} />
        </button>
      </span>
    {:else}
      <button type="button" class="add" onclick={openAdd}>+ Add filter</button>
    {/if}
  </div>

  <!--
    The input sizes itself (`flex: 1; min-width: 260px`). It used to sit in a
    hardcoded 220px box, which is what clipped long suggestions — and the
    placeholder was hardcoded too, which is how a page could advertise a
    prefix its resource does not declare. Both are now the component's job.
  -->
  <div class="right">
    <SearchAutocompleteInput bind:value={search} appId={appId ?? ''} {context} {error} {onSearch} />
    {#if showRange}
      <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} {ranges} />
    {/if}
    {@render actions?.()}
  </div>
</div>

<style>
  .filterbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 16px;
  }
  .chips { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .chip {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 6px 4px 10px;
    background: var(--primary-soft); color: var(--primary);
    border: 1px solid var(--primary-border); border-radius: var(--radius);
    font-size: 12.5px;
  }
  .c-op { opacity: 0.75; }
  .c-x, .d-x {
    display: inline-flex; align-items: center;
    background: none; border: none; color: inherit; padding: 2px; opacity: 0.7;
  }
  .c-x:hover { opacity: 1; }
  .draft {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 6px; border: 1px solid var(--border-strong); border-radius: var(--radius);
    background: var(--surface-2);
  }
  .draft select, .draft input {
    background: var(--surface); color: var(--text);
    border: 1px solid var(--border); border-radius: var(--radius-sm);
    padding: 4px 6px; font-size: 12.5px;
  }
  .draft input { width: 130px; }
  .draft input.tag-key { width: 90px; }
  .draft input.tag-val { width: 110px; }
  .tag-eq { opacity: 0.6; }
  .d-ok, .add {
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: var(--radius-sm); color: var(--text-muted);
    padding: 5px 10px; font-size: 12.5px; font-weight: 540;
  }
  .add:hover, .d-ok:hover { color: var(--text); border-color: var(--border-strong); }
  .right { display: flex; align-items: center; gap: 10px; flex: 1; min-width: 320px; justify-content: flex-end; }
</style>
