<!--
  The window every analytics page is filtered by: four rolling presets plus a
  Custom chip that opens a calendar.

  The value is a `DateRangeValue`, not a bare day count, because "12 August" and
  "the last 30 days" are different KINDS of window and collapsing them into one
  number is what made a custom range inexpressible in the first place.

  Selection is shared and persisted by `stores/range.svelte.ts`; this component
  only reports what was clicked. Pages stay in control of their own fallback,
  which is why nothing here reads the store directly.
-->
<script lang="ts">
  import DateRangePicker from './DateRangePicker.svelte';
  import Icon from './ui/Icon.svelte';
  import { t, localeStore, intlTag } from '../i18n';
  import { formatAbsolute, lastDays, type DateRangeValue } from '../models/date-range';

  interface Preset {
    days: number;
    label: string;
  }

  interface Props {
    value: DateRangeValue;
    onchange: (v: DateRangeValue) => void;
    /** Overrides the four defaults — `Issues` carries its own wider set. */
    ranges?: Preset[];
  }

  const DEFAULT: Preset[] = [
    { days: 1, label: '24h' },
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  let { value, onchange, ranges = DEFAULT }: Props = $props();

  let open = $state(false);
  let customEl = $state<HTMLButtonElement | null>(null);

  const isCustom = $derived(value.kind === 'absolute');
  /**
   * The chip's own label carries the selected window when there is one, so the
   * control always states what it is applying. A chip reading only "Custom"
   * beside a dashboard showing July would leave the reader to guess.
   */
  const customLabel = $derived(
    value.kind === 'absolute'
      ? formatAbsolute(value, intlTag(localeStore.locale))
      : t('dateRange.custom'),
  );
</script>

<div class="ranges" role="tablist">
  {#each ranges as r (r.days)}
    {@const active = value.kind === 'last' && value.days === r.days}
    <button
      class="range"
      class:active
      onclick={() => onchange(lastDays(r.days))}
      type="button"
      role="tab"
      aria-selected={active}
    >
      {r.label}
    </button>
  {/each}
  <button
    class="range custom"
    class:active={isCustom}
    type="button"
    role="tab"
    aria-selected={isCustom}
    aria-haspopup="dialog"
    aria-expanded={open}
    bind:this={customEl}
    onclick={() => (open = !open)}
    title={t('dateRange.open')}
  >
    <span class="cal" aria-hidden="true"><Icon name="calendar" size={13} /></span>
    {customLabel}
  </button>
</div>

{#if open}
  <DateRangePicker
    anchor={customEl}
    {value}
    onpick={onchange}
    onclose={() => (open = false)}
  />
{/if}

<style>
  .ranges {
    display: inline-flex;
    gap: 4px;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .range {
    padding: 6px 13px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    border-radius: var(--radius-sm);
  }
  .range:hover {
    color: var(--text);
  }
  .range.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .custom {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    /* The label grows with the selection ("July 2026"), so it needs a ceiling
       or a long custom span pushes the rest of the toolbar off-screen. */
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cal {
    display: inline-flex;
    color: var(--text-faint);
  }
  .custom.active .cal {
    color: inherit;
  }
</style>
