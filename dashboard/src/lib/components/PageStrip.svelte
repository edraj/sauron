<script lang="ts">
  import { t, formatNumber } from '../i18n';
  import Icon from './ui/Icon.svelte';
  import { pageWindow } from '../models/page-window';

  /**
   * The pager bar every table shares: a caption, Prev/Next, and a strip of
   * page numbers.
   *
   * Presentational only. It knows a page number, a page count and a callback
   * taking a page number — never an offset, a cursor, or which of the two the
   * caller will use to get there. That split is what lets the offset pager and
   * the keyset pager render byte-identical controls without either one growing
   * a branch for the other's navigation model.
   */
  interface Props {
    /** 1-based current page. */
    page: number;
    /**
     * Pages known to exist.
     *
     * A LOWER BOUND when the server capped its count, so the adapter derives
     * it as `max(ceil(total / limit), page)` — never below the page being
     * shown, or the strip renders a current page past its own last slot.
     */
    totalPages: number;
    /**
     * Whether a page exists after this one.
     *
     * Passed rather than derived from `page < totalPages`, because those
     * disagree at the count cap: with 50,000 matching rows and a total capped
     * at 10,000, page 200 is the last *numbered* page and still has rows after
     * it. The caller's `next_cursor` (or its `limit + 1` probe) is the only
     * thing that actually knows.
     */
    canNext: boolean;
    /** Caption on the left — a row range or a count. `null` renders nothing. */
    label?: string | null;
    /**
     * A load is in flight — keep the control in place, but refuse clicks.
     *
     * Pass the WHOLE in-flight window, page moves included, not just background
     * revalidates. A pager that unmounts while the next page loads and reappears
     * when it lands is a control that jumps out from under the cursor, and it
     * only does it in one direction (a cached Prev repaints instantly), so it
     * reads as a control that breaks at random. It stays mounted and goes dead
     * instead.
     */
    busy?: boolean;
    /** Called with the 1-based page to move to. Never called with the current page. */
    onjump: (page: number) => void;
  }

  let { page, totalPages, canNext, label = null, busy = false, onjump }: Props = $props();

  const canPrev = $derived(page > 1);
  const slots = $derived(totalPages > 1 ? pageWindow(page, totalPages) : []);

  /**
   * Digits in the largest page number, which sizes EVERY slot in the strip.
   *
   * Without it the slots size themselves and the strip changes width as the
   * window moves over numbers of different lengths — measured at 275.7px on
   * page 1 of a 200-page list against 332.95px on page 200, because a 1-digit
   * button sits on its 32px floor while a 3-digit one grows to 46.31px. The
   * strip is right-anchored, so Next holds still and PREV slides ~57px instead.
   * A constant slot count alone does not buy a constant width, which is a thing
   * only a browser can tell you — the unit test asserting `length === 7` passes
   * either way.
   */
  const slotDigits = $derived(String(Math.max(totalPages, 1)).length);

  function go(to: number) {
    if (busy || to === page || to < 1) return;
    onjump(to);
  }
</script>

<div class="pager">
  <span class="range muted">{label ?? ''}</span>

  <div class="nav">
    <button class="pg" disabled={!canPrev || busy} onclick={() => go(page - 1)} type="button">
      <Icon name="chevron-left" size={14} /> {t('ui.pager.prev')}
    </button>

    <div class="strip" style="--pg-digits: {slotDigits}">
      {#each slots as slot, i (i)}
        {#if slot === 'gap'}
          <span class="pg gap" aria-hidden="true">…</span>
        {:else}
          <button
            class="pg num"
            class:active={slot === page}
            aria-current={slot === page ? 'page' : undefined}
            aria-label={t('ui.pager.page', { n: slot })}
            disabled={busy}
            onclick={() => go(slot)}
            type="button">{slot}</button
          >
        {/if}
      {/each}
    </div>

    <!-- The strip collapses to this below 640px; both are never shown at once. -->
    <span class="compact muted">{t('ui.pager.pageOf', { page: formatNumber(page), total: formatNumber(totalPages) })}</span>

    <button class="pg" disabled={!canNext || busy} onclick={() => go(page + 1)} type="button">
      {t('ui.pager.next')} <Icon name="chevron-right" size={14} />
    </button>
  </div>
</div>

<style>
  /*
   * A footer BAR, not a row floated under the table.
   *
   * The old rule was `padding: 10px 2px 0` with no border, which read as
   * whatever sat above it and put the controls at a 2px inset that lined up
   * with nothing. The 18px here is `Card`'s own `card-head` inset, so inside a
   * `padding="none"` card — what every table view uses — the pager's controls
   * sit on the same vertical line as the card's title, and the border-top
   * mirrors `card-head`'s border-bottom.
   */
  .pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 18px;
    border-top: 1px solid var(--border);
    /*
     * Fixed height so the bar does not resize when `label` is null — which is
     * the whole duration of every page move, since a count that describes rows
     * no longer on screen is not a count the caller can stand behind.
     */
    min-height: 56px;
    box-sizing: border-box;
  }
  .range {
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
  }
  .nav {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .strip {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .pg {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 6px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 550;
    transition: color 0.12s ease, border-color 0.12s ease;
  }
  .pg.num {
    padding: 6px 10px;
    /*
     * Sized to the widest page number, not to this button's own. `1ch` is the
     * zero glyph's advance and `tabular-nums` makes every digit share it (which
     * is why that declaration is load bearing here, not merely tidy), plus 20px
     * padding and 2px border under `border-box`.
     *
     * 23px, not 22px: `1ch` measures marginally narrower than the tabular
     * advance in this face, so at 22px a 3-digit label overflowed its own floor
     * by 0.11px and the strip still drifted — 0.44px, invisible but not
     * actually constant. The extra pixel keeps `min-width` strictly the larger
     * value, so every slot is identical by construction rather than by rounding.
     */
    min-width: calc(var(--pg-digits, 1) * 1ch + 23px);
    justify-content: center;
    font-variant-numeric: tabular-nums;
  }
  .pg:hover:not(:disabled, .active, .gap) {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .pg.active {
    background: var(--surface-3);
    color: var(--text);
    border-color: var(--border-strong);
    cursor: default;
  }
  .pg:disabled {
    opacity: 0.4;
    cursor: default;
  }
  /*
   * A gap keeps a number button's footprint but none of its affordances, so
   * the strip stays an even grid and nothing in it looks clickable that is not.
   */
  .pg.gap {
    padding: 6px 10px;
    min-width: calc(var(--pg-digits, 1) * 1ch + 23px);
    justify-content: center;
    background: transparent;
    border-color: transparent;
    user-select: none;
  }
  .compact {
    display: none;
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
    padding: 0 4px;
  }
  @media (max-width: 640px) {
    .strip {
      display: none;
    }
    .compact {
      display: inline;
    }
  }
</style>
