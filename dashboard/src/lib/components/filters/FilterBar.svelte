<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import SearchInput from '../SearchInput.svelte';
  import DateRange from '../DateRange.svelte';
  import { OP_LABEL, composeTag, type FieldDef, type Filter, type Op } from './filters';

  interface Props {
    fields: FieldDef[];
    filters: Filter[];
    search: string;
    sinceDays: number;
    // Optional custom date-range options; falls back to DateRange's default.
    ranges?: { days: number; label: string }[];
  }
  let {
    fields,
    filters = $bindable([]),
    search = $bindable(''),
    sinceDays = $bindable(30),
    ranges = undefined,
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
      return;
    }
    if (!draftField || draftValue === '') return;
    filters = [...filters, { field: draftField, op: draftOp, value: draftValue }];
    adding = false;
  }
  function remove(i: number) {
    filters = filters.filter((_, idx) => idx !== i);
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
          <input type="number" bind:value={draftValue} placeholder="value" aria-label="Value" />
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

  <div class="right">
    <SearchInput bind:value={search} placeholder="Search…" width="220px" />
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} {ranges} />
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
  .right { display: flex; align-items: center; gap: 10px; }
</style>
