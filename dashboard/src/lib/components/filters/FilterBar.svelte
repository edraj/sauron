<!--
  The query-language list toolbar: filter chips, the search box and the window.

  Layout is `ListToolbar`'s, not this file's — every list page shares that row
  so the search box has the same place and the same neighbours everywhere.
  This component only decides what goes in each slot: "+ Add filter" leads,
  the query box takes the middle, the range pills follow unless the page
  brings its own `TimeFilter` through `actions`, and the chips plus the draft
  editor go on the second line, which `ListToolbar` renders only while there is
  something on it.

  The chips used to share the first line with the search box, to its left. Each
  chip added pushed the box sideways and shrank it, and the draft editor —
  three selects and a text field — did the same while open. Moving them under
  the row keeps the box where the eye expects it.
-->
<script lang="ts">
  import { t } from '../../i18n';
  import type { Snippet } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import ListToolbar from '../ListToolbar.svelte';
  import SearchAutocompleteInput from '../search/SearchAutocompleteInput.svelte';
  import DateRange from '../DateRange.svelte';
  import { rangeStore } from '../../stores/range.svelte';
  import { lastDays, type DateRangeValue } from '../../models/date-range';
  import {
    opLabel,
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
    /** The window the page is filtered by. Bindable — the chip strip writes it. */
    range: DateRangeValue;
    // Optional custom date-range options; falls back to DateRange's default.
    ranges?: { days: number; label: string }[];
    /**
     * Render the built-in date range.
     *
     * `false` for a page that owns its window through a richer control (a
     * `<TimeFilter>`, which also picks the timestamp COLUMN and accepts
     * absolute bounds). Those pages still pass `range` — the prop is
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
    range = $bindable(lastDays(30)),
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
  /** Whether the second line has anything to show. */
  const hasBelow = $derived(filters.length > 0 || adding);

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
    const def = fields.find((f) => f.key === key);
    return def ? t(def.labelKey) : key;
  }
</script>

{#snippet leadSlot()}
  <!-- A toggle, not a one-way opener: pressing it again while the draft is
       open closes the draft, which is what the second click means. -->
  <button
    type="button"
    class="add"
    class:active={adding}
    aria-expanded={adding}
    onclick={() => (adding ? (adding = false) : openAdd())}
  >
    {t('filter.addFilter')}
  </button>
{/snippet}

{#snippet searchSlot()}
  <SearchAutocompleteInput bind:value={search} appId={appId ?? ''} {context} {error} {onSearch} />
{/snippet}

{#snippet rangeSlot()}
  <DateRange
    value={range}
    onchange={(v) => {
      range = v;
      rangeStore.set(v);
    }}
    {ranges}
  />
{/snippet}

{#snippet chipsSlot()}
  {#each filters as f, i (i)}
    <span class="chip">
      <span class="c-field">{labelFor(f.field)}</span>
      <span class="c-op">{opLabel(f.op)}</span>
      <span class="c-val mono">{f.value}</span>
      <button type="button" class="c-x" aria-label={t('filter.remove')} onclick={() => remove(i)}>
        <Icon name="x" size={12} />
      </button>
    </span>
  {/each}

  {#if adding}
    <!-- A form, so Enter in any of its fields commits the chip the way the Add
         button does, without a keydown handler on every control. -->
    <form
      class="draft"
      aria-label={t('filter.addFilter')}
      onsubmit={(e) => {
        e.preventDefault();
        commit();
      }}
    >
      <select bind:value={draftField} onchange={onFieldChange} aria-label={t('filter.field')}>
        {#each fields as f (f.key)}<option value={f.key}>{t(f.labelKey)}</option>{/each}
      </select>
      <select bind:value={draftOp} aria-label={t('filter.operator')}>
        {#each fieldDef?.ops ?? [] as op (op)}<option value={op}>{opLabel(op)}</option>{/each}
      </select>
      {#if fieldDef?.type === 'tag'}
        <input type="text" bind:value={draftTagKey} placeholder={t('filter.placeholder.key')} aria-label={t('filter.tagKey')} class="tag-key" />
        <span class="tag-eq">=</span>
        <input type="text" bind:value={draftTagVal} placeholder={t('filter.placeholder.value')} aria-label={t('filter.tagValue')} class="tag-val" />
      {:else if fieldDef?.type === 'enum'}
        <select bind:value={draftValue} aria-label={t('filter.value')}>
          {#each fieldDef?.options ?? [] as opt (opt)}<option value={opt}>{opt}</option>{/each}
        </select>
      {:else if fieldDef?.type === 'number'}
        <!-- Text, not type="number". `bind:value` on a numberlike input
             writes back a number (or null once cleared) rather than the
             string `Filter.value` is declared as, which is what let a
             cleared field commit `times_seen:eq:null`. -->
        <input type="text" inputmode="numeric" bind:value={draftValue} placeholder={t('filter.placeholder.value')} aria-label={t('filter.value')} />
      {:else}
        <input type="text" bind:value={draftValue} placeholder={t('filter.placeholder.value')} aria-label={t('filter.value')} />
      {/if}
      <button type="submit" class="d-ok">{t('filter.add')}</button>
      <button type="button" class="d-x" aria-label={t('common.cancel')} onclick={() => (adding = false)}>
        <Icon name="x" size={13} />
      </button>
    </form>
  {/if}
{/snippet}

<ListToolbar
  lead={leadSlot}
  searchBox={searchSlot}
  timeWindow={showRange ? rangeSlot : undefined}
  {actions}
  below={hasBelow ? chipsSlot : undefined}
/>

<style>
  /* Same shell as the search box beside it: height, surface, border, radius. */
  .add {
    height: var(--control-h);
    padding: 0 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    white-space: nowrap;
    transition: color 0.13s ease, border-color 0.13s ease, background 0.13s ease;
  }
  .add:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .add.active {
    color: var(--primary);
    background: var(--primary-soft);
    border-color: var(--primary-border);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 30px;
    padding: 0 4px 0 10px;
    background: var(--primary-soft);
    color: var(--primary);
    border: 1px solid var(--primary-border);
    border-radius: var(--radius-sm);
    font-size: 12.5px;
  }
  .c-field {
    font-weight: 560;
  }
  .c-op {
    opacity: 0.75;
  }
  .c-x,
  .d-x {
    display: inline-flex;
    align-items: center;
    background: none;
    border: none;
    color: inherit;
    padding: 4px;
    border-radius: var(--radius-sm);
    opacity: 0.7;
  }
  .c-x:hover,
  .d-x:hover {
    opacity: 1;
    background: var(--primary-soft);
  }

  .draft {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 30px;
    padding: 2px 4px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
  }
  .draft select,
  .draft input {
    height: 24px;
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 0 6px;
    font-size: 12.5px;
  }
  .draft input {
    width: 130px;
  }
  .draft input.tag-key {
    width: 90px;
  }
  .draft input.tag-val {
    width: 110px;
  }
  .tag-eq {
    opacity: 0.6;
  }
  .d-ok {
    height: 24px;
    padding: 0 10px;
    background: var(--primary);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    color: var(--primary-contrast);
    font-size: 12.5px;
    font-weight: 560;
  }
  .d-ok:hover {
    background: var(--primary-hover);
  }
  .d-x {
    color: var(--text-muted);
  }
  .d-x:hover {
    color: var(--text);
    background: var(--surface-3);
  }
</style>
