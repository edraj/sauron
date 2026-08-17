<script lang="ts">
  /**
   * The category strip above the session `Timeline`.
   *
   * Selection is "narrow to what is lit": nothing lit means All, and a lit chip
   * is a lane you asked for. The alternative — every chip lit at rest, where the
   * first click REMOVES a lane — reads the same at a glance but inverts what a
   * click does, and there is no way to tell the two apart from the screen.
   *
   * The filter itself is replaced, never mutated: Svelte 5 proxies plain objects
   * and arrays but not `Set`, so a mutated set would filter nothing and re-render
   * nothing. Every handler here builds a new `Set` and hands back a new filter,
   * the same shape `Timeline`'s own `expanded` toggle uses.
   */
  import Icon, { type IconName } from './ui/Icon.svelte';
  import {
    NO_TIMELINE_FILTER,
    ROW_CATEGORIES,
    isTimelineFiltered,
    type RowCategory,
    type TimelineFilter,
  } from '../models/timeline-row';

  interface Props {
    /** Lane sizes over the WHOLE timeline — never the filtered subset. */
    counts: Record<RowCategory, number>;
    /** Ops present among this session's transactions, already ordered. */
    ops: { op: string; count: number }[];
    filter: TimelineFilter;
    onchange: (next: TimelineFilter) => void;
  }
  let { counts, ops, filter, onchange }: Props = $props();

  const LABEL: Record<RowCategory, string> = {
    navigation: 'Navigation',
    transaction: 'Transactions',
    event: 'Events',
    issue: 'Issues',
  };

  // The glyphs the timeline's own rail nodes use, so a chip and the rows it
  // admits read as one control rather than two vocabularies.
  const ICON: Record<RowCategory, IconName> = {
    navigation: 'compass',
    transaction: 'zap',
    event: 'diamond',
    issue: 'x',
  };

  const filtered = $derived(isTimelineFiltered(filter));

  // An empty category set means "all", so transactions are in view then too —
  // the op row must not vanish just because nothing has been clicked yet.
  const showOps = $derived(
    ops.length > 0 && (filter.categories.size === 0 || filter.categories.has('transaction')),
  );

  function toggleCategory(c: RowCategory) {
    const categories = new Set(filter.categories);
    if (categories.has(c)) categories.delete(c);
    else categories.add(c);
    // Dropping transactions out of view drops the op selection with them. Left
    // behind, it would be a live constraint with no control on screen, silently
    // reapplied the moment the transaction chip came back.
    const keepOps = categories.size === 0 || categories.has('transaction');
    onchange({ categories, ops: keepOps ? filter.ops : new Set() });
  }

  function toggleOp(op: string) {
    const next = new Set(filter.ops);
    if (next.has(op)) next.delete(op);
    else next.add(op);
    onchange({ categories: filter.categories, ops: next });
  }

  /** `''` is the blank-op bucket, not a missing value — see `transactionOp`. */
  function opLabel(op: string): string {
    return op === '' ? '(none)' : op;
  }
</script>

<div class="tf">
  <div class="tf-row" role="group" aria-label="Filter timeline by category">
    <Icon name="funnel" size={13} />
    <button
      class="chip all"
      type="button"
      onclick={() => onchange(NO_TIMELINE_FILTER)}
      disabled={!filtered}
      aria-pressed={!filtered}
    >
      All
    </button>
    <span class="sep" aria-hidden="true"></span>
    {#each ROW_CATEGORIES as c (c)}
      <button
        class="chip cat-{c}"
        type="button"
        onclick={() => toggleCategory(c)}
        aria-pressed={filter.categories.has(c)}
        class:on={filter.categories.has(c)}
        disabled={counts[c] === 0}
        title={counts[c] === 0 ? `No ${LABEL[c].toLowerCase()} in this session` : undefined}
      >
        <Icon name={ICON[c]} size={12} />
        {LABEL[c]}
        <span class="count">{counts[c]}</span>
      </button>
    {/each}
  </div>

  {#if showOps}
    <div class="tf-row ops" role="group" aria-label="Filter transactions by op">
      <span class="ops-label">op</span>
      {#each ops as o (o.op)}
        <!-- Named explicitly rather than left to name-from-contents: an op chip
             has no bare text node, only two spans, and "(none)" in particular
             is a label for an absent value that reads as nothing at all when
             announced without the word "op". -->
        <button
          class="chip cat-transaction"
          type="button"
          onclick={() => toggleOp(o.op)}
          aria-pressed={filter.ops.has(o.op)}
          aria-label={`op ${opLabel(o.op)}, ${o.count}`}
          class:on={filter.ops.has(o.op)}
        >
          <span class="mono">{opLabel(o.op)}</span>
          <span class="count">{o.count}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .tf {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 14px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
    color: var(--text-faint);
  }
  .tf-row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .sep {
    width: 1px;
    align-self: stretch;
    margin: 2px 4px;
    background: var(--border);
  }
  .ops-label {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    padding-left: 3px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    font-size: 12px;
    font-weight: 560;
    /* The border width is constant across every state and only its COLOUR
       changes: swapping to `border: none` when a chip lights up would shift
       every chip after it by 2px on each click. */
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--surface-2);
    color: var(--text-muted);
    transition: color 0.12s ease, background 0.12s ease, border-color 0.12s ease;
  }
  .chip:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .chip:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .count {
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: currentColor;
    opacity: 0.7;
  }
  .all[aria-pressed='true'] {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
    opacity: 1;
  }
  /* Lit chips borrow the tone of the row badge they admit, so the strip carries
     the same colour vocabulary as the rows below it. */
  .chip.on {
    border-color: transparent;
  }
  .cat-navigation.on {
    color: var(--warning);
    background: var(--warning-soft);
  }
  .cat-transaction.on {
    color: var(--info);
    background: var(--info-soft);
  }
  .cat-event.on {
    color: var(--primary);
    background: var(--primary-soft);
  }
  .cat-issue.on {
    color: var(--error);
    background: var(--error-soft);
  }
</style>
