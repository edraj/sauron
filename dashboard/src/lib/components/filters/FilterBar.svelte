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
  import { t, type MessageKey } from '../../i18n';
  import type { Snippet } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import Button from '../ui/Button.svelte';
  import Select from '../ui/Select.svelte';
  import ListToolbar from '../ListToolbar.svelte';
  import { LEVEL_KEYS, STATUS_KEYS } from '../../models/issue-labels';
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

  /**
   * The words for an enum value the API speaks in, so a chip reads "Status is
   * Unresolved" rather than echoing the wire value `unresolved`. Values with
   * no entry (an `op` like `screen_load`, a free-text value) stay as typed —
   * they are identifiers, not words.
   */
  const ENUM_LABELS: Record<string, Record<string, MessageKey>> = {
    level: LEVEL_KEYS,
    status: STATUS_KEYS,
  };
  /** Own-property lookups: field and value are user/URL input. */
  function valueKey(field: string, value: string): MessageKey | undefined {
    const table = Object.prototype.hasOwnProperty.call(ENUM_LABELS, field) ? ENUM_LABELS[field] : undefined;
    return table && Object.prototype.hasOwnProperty.call(table, value) ? table[value] : undefined;
  }
  function valueLabel(field: string, value: string): string {
    const key = valueKey(field, value);
    return key ? t(key) : value;
  }

  const fieldOptions = $derived(fields.map((f) => ({ value: f.key, label: t(f.labelKey) })));
  const opOptions = $derived((fieldDef?.ops ?? []).map((op) => ({ value: op, label: opLabel(op) })));
  const enumOptions = $derived(
    (fieldDef?.options ?? []).map((opt) => ({ value: opt, label: valueLabel(draftField, opt) })),
  );
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
      <span class="c-val" class:mono={!valueKey(f.field, f.value)}>{valueLabel(f.field, f.value)}</span>
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
      <Select bind:value={draftField} options={fieldOptions} onchange={onFieldChange} ariaLabel={t('filter.field')} />
      <Select value={draftOp} options={opOptions} onchange={(v) => (draftOp = v as Op)} ariaLabel={t('filter.operator')} />
      {#if fieldDef?.type === 'tag'}
        <input type="text" bind:value={draftTagKey} placeholder={t('filter.placeholder.key')} aria-label={t('filter.tagKey')} class="tag-key" />
        <span class="tag-eq">=</span>
        <input type="text" bind:value={draftTagVal} placeholder={t('filter.placeholder.value')} aria-label={t('filter.tagValue')} class="tag-val" />
      {:else if fieldDef?.type === 'enum'}
        <Select bind:value={draftValue} options={enumOptions} ariaLabel={t('filter.value')} />
      {:else if fieldDef?.type === 'number'}
        <!-- Text, not type="number". `bind:value` on a numberlike input
             writes back a number (or null once cleared) rather than the
             string `Filter.value` is declared as, which is what let a
             cleared field commit `times_seen:eq:null`. -->
        <input type="text" inputmode="numeric" bind:value={draftValue} placeholder={t('filter.placeholder.value')} aria-label={t('filter.value')} />
      {:else}
        <input type="text" bind:value={draftValue} placeholder={t('filter.placeholder.value')} aria-label={t('filter.value')} />
      {/if}
      <Button type="submit" variant="primary">{t('filter.add')}</Button>
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
  /* `Button` secondary at `--control-h`, kept a raw <button> because it is a
     disclosure toggle (`aria-expanded`), which `Button` does not carry. Open,
     it takes the segmented control's selected fill. */
  .add {
    height: var(--control-h);
    padding: 0 12px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    transition: background-color 0.12s ease, color 0.12s ease;
  }
  .add:hover {
    background: var(--surface-2);
  }
  .add.active {
    background: var(--surface-3);
  }

  /* An applied filter: a quiet neutral token, not an accent. The field name
     is muted so the value — the part that differs between chips — leads. */
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--control-h);
    padding: 0 4px 0 10px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: var(--text-xs);
  }
  .c-field {
    color: var(--text-muted);
    font-weight: var(--weight-medium);
  }
  .c-op {
    color: var(--text-faint);
  }
  .c-val {
    font-weight: var(--weight-medium);
  }
  .c-x,
  .d-x {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--text-muted);
    border-radius: var(--radius-sm);
    transition: background-color 0.12s ease, color 0.12s ease;
  }
  .c-x {
    padding: 4px;
  }
  .c-x:hover,
  .d-x:hover {
    color: var(--text);
    background: var(--surface-3);
  }

  /* The draft is a row of ordinary controls, the same height as the chips
     beside it — no box around it. */
  .draft {
    display: inline-flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
  }
  /* `Input`'s shell for the free-text value fields (`Input` has no
     `aria-label`, which these need). */
  .draft input {
    height: var(--control-h);
    width: 150px;
    padding: 0 10px;
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    font-size: var(--text-sm);
    outline: none;
    transition: border-color 0.12s ease, box-shadow 0.12s ease;
  }
  .draft input::placeholder {
    color: var(--text-faint);
  }
  .draft input:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  .draft input.tag-key {
    width: 110px;
  }
  .draft input.tag-val {
    width: 130px;
  }
  .tag-eq {
    color: var(--text-faint);
  }
  .d-x {
    width: var(--control-h);
    height: var(--control-h);
  }
</style>
