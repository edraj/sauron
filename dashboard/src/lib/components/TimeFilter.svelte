<script lang="ts">
  import { t } from '../i18n';
  /**
   * The time window control for the signal-browsing lists.
   *
   * Replaces `DateRange` on Events, Sessions, Users and Devices. `DateRange`
   * itself stays — the chart and stat-tile cards above these tables keep their
   * own simple window, deliberately, so this control governs the TABLE only.
   *
   * Thin by design. Every rule lives in `models/time-filter.ts`, which is unit
   * tested; this file is presentation and wiring. That split is the house
   * pattern (there is no component-render harness in this project), so logic
   * added here is logic that cannot be tested.
   */
  import { untrack } from 'svelte';
  import {
    validate,
    localInputToUtc,
    utcToLocalInput,
    localZoneLabel,
    type TimeField,
    type TimeFilterState,
    type TimeMode,
  } from '../models/time-filter';

  interface Props {
    /** The columns this page can window on. One entry renders a label, not a select. */
    fields: TimeField[];
    value: TimeFilterState;
    onchange: (v: TimeFilterState) => void;
    /** Day counts offered in `last` mode. */
    presets?: number[];
  }

  let { fields, value, onchange, presets = [1, 7, 30, 90, 365] }: Props = $props();

  // `$derived`, not a plain const: `t()` reads the locale store, so a value
  // computed once at init would freeze these in the mount language.
  const MODES: { key: TimeMode; label: string }[] = $derived([
    { key: 'last', label: t('ui.time.mode.last') },
    { key: 'after', label: t('ui.time.mode.after') },
    { key: 'before', label: t('ui.time.mode.before') },
    { key: 'between', label: t('ui.time.mode.between') },
  ]);

  function presetLabel(d: number): string {
    if (d === 1) return '24h';
    if (d === 365) return '1y';
    return `${d}d`;
  }

  /**
   * A stable STRING identity for a filter.
   *
   * Compared as a string rather than by `===` on the object because `value`
   * arrives through `$state`, whose deep proxying means a reference comparison
   * never matches even when nothing changed — the sync effect below would then
   * clobber the draft on every single render.
   */
  function keyOf(v: TimeFilterState): string {
    return `${v.field}|${v.mode}|${v.lastDays ?? ''}|${v.from ?? ''}|${v.to ?? ''}`;
  }

  // The in-progress edit. A local draft is unavoidable: switching mode to
  // `between` while only `from` is set produces a transiently INVALID filter
  // that must still render so the user can fill in the other bound. Emitting it
  // would put a request on the wire the server answers with a 400.
  // svelte-ignore state_referenced_locally
  // Seeding only, and the divergence is the entire point — see the comment
  // above. The `$effect` below is what re-syncs on a genuine OUTSIDE change,
  // deliberately and not on every read.
  let draft = $state<TimeFilterState>({ ...value });
  // svelte-ignore state_referenced_locally
  let syncedKey = $state(keyOf(value));

  // Re-sync only when `value` genuinely changed from OUTSIDE — a URL restore,
  // or a page resetting its filter. `syncedKey` is read through `untrack` so
  // this effect depends on `value` alone; without that it would re-run on its
  // own write and fight the draft it just accepted.
  $effect(() => {
    const k = keyOf(value);
    if (k !== untrack(() => syncedKey)) {
      syncedKey = k;
      draft = { ...value };
    }
  });

  const error = $derived(validate(draft));
  const zone = $derived(localZoneLabel());
  const fromInput = $derived(draft.from ? utcToLocalInput(draft.from) : '');
  const toInput = $derived(draft.to ? utcToLocalInput(draft.to) : '');

  /** Whether `lastDays` is a value the preset select can show. */
  const isPreset = $derived(draft.mode === 'last' && presets.includes(draft.lastDays ?? -1));
  let custom = $state(false);

  function commit(next: TimeFilterState) {
    draft = next;
    if (validate(next) === null) {
      syncedKey = keyOf(next);
      onchange(next);
    }
  }

  function setField(e: Event) {
    commit({ ...draft, field: (e.currentTarget as HTMLSelectElement).value });
  }

  function setMode(e: Event) {
    const mode = (e.currentTarget as HTMLSelectElement).value as TimeMode;
    custom = false;
    // Dropping the bounds the new mode cannot use is what keeps `toParams`
    // honest: a leftover `to` on an `after` filter would be sent and would
    // silently narrow a window the control no longer shows.
    if (mode === 'last') {
      commit({ field: draft.field, mode, lastDays: draft.lastDays ?? 30 });
    } else if (mode === 'after') {
      commit({ field: draft.field, mode, from: draft.from });
    } else if (mode === 'before') {
      commit({ field: draft.field, mode, to: draft.to });
    } else {
      commit({ field: draft.field, mode, from: draft.from, to: draft.to });
    }
  }

  function setPreset(e: Event) {
    const raw = (e.currentTarget as HTMLSelectElement).value;
    if (raw === 'custom') {
      custom = true;
      return;
    }
    custom = false;
    commit({ field: draft.field, mode: 'last', lastDays: Number(raw) });
  }

  function setCustomDays(e: Event) {
    const raw = (e.currentTarget as HTMLInputElement).value.trim();
    // Parsed from the STRING, not read off a number input. See the `type`
    // attribute's comment in the markup below.
    const n = /^\d+$/.test(raw) ? Number(raw) : NaN;
    commit({ field: draft.field, mode: 'last', lastDays: n });
  }

  function setBound(which: 'from' | 'to', e: Event) {
    const iso = localInputToUtc((e.currentTarget as HTMLInputElement).value, which);
    commit({ ...draft, [which]: iso ?? undefined });
  }
</script>

<div class="timefilter">
  {#if fields.length > 1}
    <select value={draft.field} onchange={setField} aria-label={t('ui.time.field')}>
      {#each fields as f (f.key)}<option value={f.key}>{f.label}</option>{/each}
    </select>
  {:else if fields.length === 1}
    <!-- A select with a single option is a control that cannot be operated. -->
    <span class="static-field">{fields[0].label}</span>
  {/if}

  <select value={draft.mode} onchange={setMode} aria-label={t('ui.time.comparison')}>
    {#each MODES as m (m.key)}<option value={m.key}>{m.label}</option>{/each}
  </select>

  {#if draft.mode === 'last'}
    <select
      value={custom || !isPreset ? 'custom' : String(draft.lastDays)}
      onchange={setPreset}
      aria-label={t('ui.time.range')}
    >
      {#each presets as d (d)}<option value={String(d)}>{presetLabel(d)}</option>{/each}
      <option value="custom">{t('ui.time.custom')}</option>
    </select>
    {#if custom || !isPreset}
      <!-- TEXT, never type="number". `bind:value` on a numberlike input writes
           back `number | null` rather than the string this reads, and because
           `error` below is a `$derived` consumed by the markup, a validator
           throwing on the wrong type takes the whole render down — the control
           freezes rather than showing a message. A number input also silently
           rounds a mistyped `3.0` to 3. Same reasoning as `FilterBar`'s
           numeric filter value. -->
      <input
        type="text"
        inputmode="numeric"
        class="days"
        value={draft.lastDays ?? ''}
        onchange={setCustomDays}
        aria-label={t('ui.time.numberOfDays')}
      />
      <span class="unit">days</span>
    {/if}
  {:else}
    {#if draft.mode !== 'before'}
      <input
        type="datetime-local"
        value={fromInput}
        onchange={(e) => setBound('from', e)}
        aria-label={t('ui.time.from')}
      />
    {/if}
    {#if draft.mode === 'between'}<span class="unit">to</span>{/if}
    {#if draft.mode !== 'after'}
      <input
        type="datetime-local"
        value={toInput}
        onchange={(e) => setBound('to', e)}
        aria-label="To"
      />
    {/if}
    <!-- The offset is shown because the value entered is read in the viewer's
         own zone. Without it an absolute window is ambiguous by exactly the
         offset, which is the kind of error that looks like missing data. -->
    <span class="tz" title={t('ui.time.localTimezone')}>{zone}</span>
  {/if}

  {#if error}
    <span class="err" role="status">{error}</span>
  {/if}
</div>

<style>
  .timefilter {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    padding: 4px 6px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .timefilter select,
  .timefilter input {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 4px 6px;
    font-size: 12.5px;
  }
  .static-field,
  .unit {
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    padding: 0 2px;
  }
  .static-field {
    color: var(--text);
  }
  .days {
    width: 62px;
  }
  .tz {
    color: var(--text-muted);
    font-size: 11.5px;
    padding: 0 2px;
    white-space: nowrap;
  }
  .err {
    color: var(--danger, #b4232a);
    font-size: 11.5px;
    padding: 0 2px;
  }
</style>
