<!--
  The calendar behind the range strip's "Custom" chip.

  Four ways to pick, because the question people actually ask is usually a
  CALENDAR unit — "what happened on the 12th", "how did last week go", "show me
  July" — and expressing those as two date inputs makes the user do arithmetic
  the app already knows how to do.

  Portalled to <body>, positioned against the trigger, and dismissed on outside
  pointerdown or Escape — the same pattern `SwitcherMenu.svelte` established, and
  for the same reason: an ancestor with `overflow: hidden` would otherwise clip
  it.

  Everything about what a window IS lives in `models/date-range.ts`; this file
  only decides which day the user meant.
-->
<script lang="ts">
  import Icon from './ui/Icon.svelte';
  import { t, localeStore, intlTag, isRtl } from '../i18n';
  import {
    MAX_RANGE_DAYS,
    customRange,
    dayRange,
    monthRange,
    weekRange,
    type AbsolutePreset,
    type DateRangeValue,
  } from '../models/date-range';
  import { rangeStore } from '../stores/range.svelte';

  interface Props {
    /** The control the popover anchors to. */
    anchor: HTMLElement | null;
    /** The window currently applied, so it can be highlighted. */
    value: DateRangeValue;
    onpick: (v: DateRangeValue) => void;
    onclose: () => void;
  }

  let { anchor, value, onpick, onclose }: Props = $props();

  type Mode = AbsolutePreset;
  let mode = $state<Mode>(value.kind === 'absolute' ? value.preset : 'day');
  /** First endpoint of an in-progress `range` selection. */
  let pendingStart = $state<Date | null>(null);
  let warning = $state<string | null>(null);

  let panelEl = $state<HTMLDivElement | null>(null);
  let pos = $state({ top: 0, left: 0 });

  /** The month on screen. Day 1 so month arithmetic never overflows. */
  let cursor = $state(startOfMonth(value.kind === 'absolute' ? new Date(value.from) : new Date()));

  const tag = $derived(intlTag(localeStore.locale));
  const rtl = $derived(isRtl(localeStore.locale));

  /**
   * Which weekday a week starts on, in the active locale.
   *
   * `Intl.Locale.prototype.getWeekInfo` is the right answer and is not
   * available everywhere, so a small table backs it: Sunday for English,
   * Saturday for Arabic. Hard-coding Monday would put the wrong seven days
   * under the word "week" for both of the locales this app actually ships.
   */
  const weekStartsOn = $derived.by(() => {
    try {
      const info = (
        new Intl.Locale(tag) as Intl.Locale & { getWeekInfo?: () => { firstDay: number } }
      ).getWeekInfo?.();
      // `getWeekInfo` numbers Monday=1 … Sunday=7; `Date.getDay()` numbers
      // Sunday=0 … Saturday=6. Converting is not optional.
      if (info) return info.firstDay % 7;
    } catch {
      /* fall through to the table */
    }
    return localeStore.locale === 'ar' ? 6 : 0;
  });

  function startOfMonth(d: Date): Date {
    return new Date(d.getFullYear(), d.getMonth(), 1, 0, 0, 0, 0);
  }
  function midnight(d: Date, plus = 0): Date {
    return new Date(d.getFullYear(), d.getMonth(), d.getDate() + plus, 0, 0, 0, 0);
  }
  function sameDay(a: Date, b: Date): boolean {
    return midnight(a).getTime() === midnight(b).getTime();
  }

  const today = midnight(new Date());

  /**
   * Six weeks of seven days covering `cursor`'s month, including the leading
   * and trailing days that belong to its neighbours.
   *
   * Always six rows, never five-or-six: a grid that changes height as you page
   * through months makes the footer jump under the pointer.
   */
  const weeks = $derived.by(() => {
    const first = startOfMonth(cursor);
    const lead = (first.getDay() - weekStartsOn + 7) % 7;
    const start = midnight(first, -lead);
    const out: Date[][] = [];
    for (let w = 0; w < 6; w++) {
      out.push(Array.from({ length: 7 }, (_, i) => midnight(start, w * 7 + i)));
    }
    return out;
  });

  const weekdayNames = $derived.by(() => {
    const fmt = new Intl.DateTimeFormat(tag, { weekday: 'short' });
    // Any week works — 4 Jan 1970 was a Sunday, so `+ i` walks the whole cycle.
    return Array.from({ length: 7 }, (_, i) =>
      fmt.format(new Date(1970, 0, 4 + ((weekStartsOn + i) % 7))),
    );
  });

  const monthLabel = $derived(
    new Intl.DateTimeFormat(tag, { month: 'long', year: 'numeric' }).format(cursor),
  );
  const dayNum = $derived.by(() => {
    const fmt = new Intl.DateTimeFormat(tag, { day: 'numeric' });
    return (d: Date) => fmt.format(d);
  });

  /** The window currently applied, as millisecond bounds, for highlighting. */
  const applied = $derived(
    value.kind === 'absolute'
      ? { from: new Date(value.from).getTime(), to: new Date(value.to).getTime() }
      : null,
  );

  function inApplied(d: Date): boolean {
    if (!applied) return false;
    const t0 = midnight(d).getTime();
    return t0 >= applied.from && t0 < applied.to;
  }

  /** The half-open span a hovered/selected cell would produce, for preview. */
  function previewOf(d: Date): DateRangeValue | null {
    switch (mode) {
      case 'day':
        return dayRange(d);
      case 'week':
        return weekRange(d, weekStartsOn);
      case 'month':
        return monthRange(d);
      case 'custom':
        return pendingStart ? customRange(pendingStart, d) : null;
    }
  }

  function commit(v: DateRangeValue): void {
    if (v.kind === 'absolute') {
      const span = new Date(v.to).getTime() - new Date(v.from).getTime();
      if (span > MAX_RANGE_DAYS * 86_400_000) {
        // The server REFUSES an over-wide explicit window rather than
        // narrowing it, so letting this through would 400 every request the
        // page makes. Refusing here, with the reason, is the only honest
        // alternative to silently moving the user's own bound.
        warning = t('dateRange.tooWide', { days: MAX_RANGE_DAYS });
        return;
      }
    }
    warning = null;
    pendingStart = null;
    onpick(v);
    onclose();
  }

  function clickDay(d: Date): void {
    warning = null;
    if (mode === 'custom') {
      if (!pendingStart) {
        pendingStart = d;
        return;
      }
      commit(customRange(pendingStart, d));
      return;
    }
    const v = previewOf(d);
    if (v) commit(v);
  }

  function shiftMonth(by: number): void {
    cursor = new Date(cursor.getFullYear(), cursor.getMonth() + by, 1, 0, 0, 0, 0);
  }

  // -------------------------------------------------------------------------
  // Keyboard
  // -------------------------------------------------------------------------

  /** The cell that owns `tabindex="0"`. Roving, as `role="grid"` requires. */
  let focusDay = $state(
    midnight(value.kind === 'absolute' ? new Date(value.from) : new Date()),
  );

  function moveFocus(byDays: number): void {
    const next = midnight(focusDay, byDays);
    focusDay = next;
    // Paging the grid when focus leaves the visible month is what makes arrow
    // navigation continuous rather than stopping at an edge.
    if (next.getMonth() !== cursor.getMonth() || next.getFullYear() !== cursor.getFullYear()) {
      cursor = startOfMonth(next);
    }
  }

  function onGridKeydown(e: KeyboardEvent): void {
    // LOGICAL, not physical. Under `dir="rtl"` the calendar is mirrored, so
    // ArrowLeft has to advance — the dashboard ships Arabic, and getting this
    // backwards makes the whole grid feel broken rather than subtly wrong.
    const back = rtl ? 'ArrowRight' : 'ArrowLeft';
    const fwd = rtl ? 'ArrowLeft' : 'ArrowRight';
    switch (e.key) {
      case back:
        e.preventDefault();
        moveFocus(-1);
        break;
      case fwd:
        e.preventDefault();
        moveFocus(1);
        break;
      case 'ArrowUp':
        e.preventDefault();
        moveFocus(-7);
        break;
      case 'ArrowDown':
        e.preventDefault();
        moveFocus(7);
        break;
      case 'Home':
        e.preventDefault();
        moveFocus(-((focusDay.getDay() - weekStartsOn + 7) % 7));
        break;
      case 'End':
        e.preventDefault();
        moveFocus(6 - ((focusDay.getDay() - weekStartsOn + 7) % 7));
        break;
      case 'PageUp':
        e.preventDefault();
        shiftMonth(-1);
        focusDay = startOfMonth(cursor);
        break;
      case 'PageDown':
        e.preventDefault();
        shiftMonth(1);
        focusDay = startOfMonth(cursor);
        break;
      case 'Enter':
      case ' ':
        e.preventDefault();
        clickDay(focusDay);
        break;
      default:
        return;
    }
  }

  // Keep the DOM focus on whichever cell is roving, but only while the grid
  // already holds focus — moving it otherwise would steal focus from the page
  // the moment the month changed for an unrelated reason.
  $effect(() => {
    const target = panelEl?.querySelector<HTMLElement>('.cell[tabindex="0"]');
    if (target && panelEl?.contains(document.activeElement)) target.focus();
  });

  // -------------------------------------------------------------------------
  // Placement and dismissal
  // -------------------------------------------------------------------------

  function reposition(): void {
    if (!anchor) return;
    const r = anchor.getBoundingClientRect();
    const width = panelEl?.offsetWidth ?? 320;
    // Clamped into the viewport rather than allowed to overflow: the trigger
    // sits at the right edge of a toolbar on most of these pages, and an RTL
    // layout puts it at the left.
    const left = Math.min(Math.max(8, r.left), window.innerWidth - width - 8);
    pos = { top: r.bottom + 6, left };
  }

  $effect(() => {
    reposition();
    function onDocPointer(e: PointerEvent) {
      const target = e.target as Node;
      if (anchor?.contains(target) || panelEl?.contains(target)) return;
      onclose();
    }
    function onDocKeydown(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onclose();
        anchor?.focus();
      }
    }
    document.addEventListener('pointerdown', onDocPointer, true);
    document.addEventListener('keydown', onDocKeydown);
    window.addEventListener('resize', reposition);
    window.addEventListener('scroll', reposition, true);
    return () => {
      document.removeEventListener('pointerdown', onDocPointer, true);
      document.removeEventListener('keydown', onDocKeydown);
      window.removeEventListener('resize', reposition);
      window.removeEventListener('scroll', reposition, true);
    };
  });

  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      },
    };
  }

  // -------------------------------------------------------------------------
  // Saved ranges
  // -------------------------------------------------------------------------

  function saveCurrent(): void {
    if (value.kind !== 'absolute') return;
    const name = window.prompt(t('dateRange.namePrompt'));
    if (name) rangeStore.save(name, value);
  }

  const MODES: Mode[] = ['day', 'week', 'month', 'custom'];
  const MODE_KEYS = {
    day: 'dateRange.mode.day',
    week: 'dateRange.mode.week',
    month: 'dateRange.mode.month',
    custom: 'dateRange.mode.range',
  } as const;
</script>

<div
  class="panel"
  bind:this={panelEl}
  use:portal
  style="top:{pos.top}px; left:{pos.left}px"
  dir={rtl ? 'rtl' : 'ltr'}
  role="dialog"
  aria-label={t('dateRange.open')}
>
  <div class="modes" role="tablist" aria-label={t('dateRange.open')}>
    {#each MODES as m (m)}
      <button
        type="button"
        class="mode"
        class:active={mode === m}
        role="tab"
        aria-selected={mode === m}
        onclick={() => {
          mode = m;
          pendingStart = null;
          warning = null;
        }}
      >
        {t(MODE_KEYS[m])}
      </button>
    {/each}
  </div>

  <div class="head">
    <button type="button" class="nav" onclick={() => shiftMonth(-1)} aria-label={t('dateRange.prevMonth')}>
      <Icon name={rtl ? 'chevron-right' : 'chevron-left'} size={15} />
    </button>
    {#if mode === 'month'}
      <button type="button" class="month-pick" onclick={() => commit(monthRange(cursor))} title={t('dateRange.selectMonth')}>
        {monthLabel}
      </button>
    {:else}
      <span class="month">{monthLabel}</span>
    {/if}
    <button type="button" class="nav" onclick={() => shiftMonth(1)} aria-label={t('dateRange.nextMonth')}>
      <Icon name={rtl ? 'chevron-left' : 'chevron-right'} size={15} />
    </button>
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
  <div class="grid" role="grid" tabindex="-1" onkeydown={onGridKeydown}>
    <div class="row headrow" role="row">
      {#each weekdayNames as name (name)}
        <span class="wd" role="columnheader" aria-label={name}>{name}</span>
      {/each}
    </div>
    {#each weeks as week (week[0].getTime())}
      <div class="row" role="row">
        {#each week as d (d.getTime())}
          {@const other = d.getMonth() !== cursor.getMonth()}
          {@const future = d.getTime() > today.getTime()}
          <button
            type="button"
            class="cell"
            class:other
            class:today={sameDay(d, today)}
            class:applied={inApplied(d)}
            class:pending={pendingStart != null && sameDay(d, pendingStart)}
            role="gridcell"
            tabindex={sameDay(d, focusDay) ? 0 : -1}
            aria-selected={inApplied(d)}
            disabled={future}
            title={future ? t('dateRange.future') : undefined}
            onclick={() => clickDay(d)}
            onfocus={() => (focusDay = d)}
          >
            {dayNum(d)}
          </button>
        {/each}
      </div>
    {/each}
  </div>

  {#if mode === 'custom'}
    <p class="hint">{pendingStart ? t('dateRange.pickEnd') : t('dateRange.pickStart')}</p>
  {/if}
  {#if warning}
    <p class="warn" role="alert">{warning}</p>
  {/if}

  {#if rangeStore.saved.length > 0}
    <div class="saved">
      <span class="saved-title">{t('dateRange.saved')}</span>
      {#each rangeStore.saved as s (s.id)}
        <div class="saved-row">
          <!--
            Stripped back to a plain range, NOT handed over whole: a
            `SavedRange` also carries `id` and `name`, and committing it would
            persist those into the CURRENT selection — which then keeps the
            name of a saved range the user may go on to delete.
          -->
          <button
            type="button"
            class="saved-pick"
            onclick={() => commit({ kind: 'absolute', from: s.from, to: s.to, preset: s.preset })}
          >
            {s.name}
          </button>
          <button
            type="button"
            class="saved-x"
            onclick={() => rangeStore.remove(s.id)}
            aria-label={t('dateRange.removeSaved')}
            title={t('dateRange.removeSaved')}
          >
            <Icon name="circle-x" size={13} />
          </button>
        </div>
      {/each}
    </div>
  {/if}

  {#if value.kind === 'absolute'}
    <button type="button" class="save" onclick={saveCurrent}>{t('dateRange.saveThis')}</button>
  {/if}
</div>

<style>
  .panel {
    position: fixed;
    z-index: 120;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    padding: 10px;
    width: 288px;
    max-height: min(560px, calc(100vh - 80px));
    overflow-y: auto;
  }
  .modes {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    margin-bottom: 8px;
  }
  .mode {
    flex: 1;
    padding: 5px 6px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12px;
    font-weight: 560;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .mode:hover {
    color: var(--text);
  }
  .mode.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    margin-bottom: 6px;
  }
  .nav {
    display: inline-flex;
    align-items: center;
    padding: 4px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    cursor: pointer;
  }
  .nav:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .month,
  .month-pick {
    flex: 1;
    text-align: center;
    font-size: 13px;
    font-weight: 620;
    color: var(--text);
  }
  .month-pick {
    border: 1px dashed var(--border-strong);
    background: transparent;
    border-radius: var(--radius-sm);
    padding: 4px 6px;
    cursor: pointer;
  }
  .month-pick:hover {
    background: var(--surface-2);
  }
  .row {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 2px;
  }
  .wd {
    text-align: center;
    font-size: 10.5px;
    font-weight: 620;
    color: var(--text-faint);
    padding: 3px 0;
  }
  .cell {
    aspect-ratio: 1;
    border: 1px solid transparent;
    background: transparent;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
  }
  .cell:hover:not(:disabled) {
    background: var(--surface-2);
    color: var(--text);
  }
  .cell:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: -2px;
  }
  .cell.other {
    color: var(--text-faint);
    opacity: 0.55;
  }
  .cell:disabled {
    cursor: not-allowed;
    opacity: 0.3;
  }
  .cell.today {
    border-color: var(--border-strong);
    font-weight: 700;
  }
  .cell.applied {
    background: var(--primary);
    color: var(--on-primary, #fff);
  }
  .cell.pending {
    border-color: var(--primary);
    color: var(--text);
  }
  .hint,
  .warn {
    margin: 8px 0 0;
    font-size: 11.5px;
  }
  .hint {
    color: var(--text-faint);
  }
  .warn {
    color: var(--danger, #c00);
  }
  .saved {
    margin-top: 10px;
    border-top: 1px solid var(--border);
    padding-top: 8px;
  }
  .saved-title {
    display: block;
    font-size: 10.5px;
    font-weight: 640;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    margin-bottom: 4px;
  }
  .saved-row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .saved-pick {
    flex: 1;
    text-align: start;
    padding: 5px 6px;
    border: none;
    background: transparent;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12.5px;
    cursor: pointer;
  }
  .saved-pick:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .saved-x {
    display: inline-flex;
    padding: 3px;
    border: none;
    background: transparent;
    color: var(--text-faint);
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .saved-x:hover {
    color: var(--danger, #c00);
  }
  .save {
    width: 100%;
    margin-top: 8px;
    padding: 6px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12px;
    font-weight: 560;
    cursor: pointer;
  }
  .save:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
</style>
