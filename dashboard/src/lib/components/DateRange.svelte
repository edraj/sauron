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
  /* Drawn exactly like `ui/SegmentedControl`: a 3px inset track on the
     control surface, `--control-h` tall, the selected segment on
     `--surface-3`. Not rendered THROUGH it because the Custom chip is not a
     plain segment — it carries an icon, anchors the calendar popover, and a
     second click on it while it is already selected must reopen the picker,
     which a radio segment ignores by design. */
  .ranges {
    display: inline-flex;
    align-items: stretch;
    height: var(--control-h);
    padding: 3px;
    gap: 2px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    flex-shrink: 0;
  }
  .range {
    display: inline-flex;
    align-items: center;
    padding: 0 10px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    transition: background-color 0.12s ease, color 0.12s ease;
  }
  .range:hover {
    color: var(--text);
  }
  .range.active {
    background: var(--surface-3);
    color: var(--text);
  }
  .custom {
    gap: 5px;
    /* The label grows with the selection ("July 2026"), so it needs a ceiling
       or a long custom span pushes the rest of the toolbar off-screen. */
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cal {
    display: inline-flex;
    color: var(--text-faint);
  }
  .custom.active .cal {
    color: inherit;
  }
</style>
